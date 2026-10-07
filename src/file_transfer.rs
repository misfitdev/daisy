//! Separate authenticated bulk connections and atomic copied-file delivery.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt as PositionalFileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, bail, ensure};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};
use xattr::FileExt;

use crate::files::{self, Item, Kind, Leases, Offer, OfferId, Part};
use crate::identity::{Identity, PublicKey};
use crate::protocol::Message;
use crate::session::Channel;

const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

pub fn supported() -> bool {
    #[cfg(all(target_os = "macos", not(test)))]
    {
        crate::macos::file_pasteboard::available()
    }
    #[cfg(any(not(target_os = "macos"), test))]
    {
        true
    }
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Clone)]
struct Entry {
    path: String,
    kind: Kind,
    len: u64,
    mode: u32,
    dev: u64,
    ino: u64,
    attributes: Vec<(String, Vec<u8>)>,
}
struct Root {
    file: File,
    entries: Vec<Entry>,
}
struct Snapshot {
    offer: Offer,
    roots: Vec<Root>,
}
struct Local {
    snapshot: Arc<Snapshot>,
    leases: Leases,
}
pub struct Remote {
    pub peer: PublicKey,
    pub offer: Offer,
    until: Mutex<Instant>,
    pub active: AtomicU64,
    pub cancelled: AtomicBool,
}

#[cfg(test)]
impl Remote {
    pub(crate) fn fixture(offer: Offer) -> Self {
        Self {
            peer: PublicKey::from_bytes(&[1; 32]).unwrap(),
            offer,
            until: Mutex::new(Instant::now() + files::TTL),
            active: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        }
    }
}

/// Native progress is separate from input capture and its queues.
#[derive(Default)]
pub struct Progress {
    pub done: AtomicU64,
    /// Shared across every item in one clipboard materialization.
    pub entries: AtomicU64,
    pub cancelled: AtomicBool,
}

pub struct Hub {
    identity: Identity,
    signer: crate::device::Signer,
    peers: crate::peers::PeerStore,
    port: u16,
    enabled: watch::Receiver<bool>,
    active: Mutex<BTreeMap<PublicKey, (IpAddr, u64)>>,
    registration: AtomicU64,
    local: Mutex<Option<Local>>,
    remote: Mutex<Option<Arc<Remote>>>,
    pub messages: mpsc::Sender<Message>,
    receiver: Mutex<Option<mpsc::Receiver<Message>>>,
    pub runtime: tokio::runtime::Handle,
    crossing: AtomicBool,
}

async fn accept_connection<T, F, Fut>(mut accept: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    loop {
        match accept().await {
            Ok(connection) => return connection,
            Err(error) => {
                tracing::warn!(?error, "copied-file listener accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

impl Hub {
    pub fn start(
        identity: Identity,
        signer: crate::device::Signer,
        peers: crate::peers::PeerStore,
        enabled: watch::Receiver<bool>,
    ) -> Result<(Arc<Self>, tokio::task::JoinHandle<()>)> {
        let socket = socket2::Socket::new(
            socket2::Domain::IPV6,
            socket2::Type::STREAM,
            Some(socket2::Protocol::TCP),
        )?;
        socket.set_only_v6(false)?;
        socket.bind(&SocketAddr::from(([0u16; 8], 0)).into())?;
        socket.listen(128)?;
        let listener: std::net::TcpListener = socket.into();
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let listener = TcpListener::from_std(listener)?;
        let (messages, receiver) = mpsc::channel(16);
        let hub = Arc::new(Self {
            identity,
            signer,
            peers,
            port,
            enabled,
            active: Mutex::default(),
            registration: AtomicU64::new(0),
            local: Mutex::default(),
            remote: Mutex::default(),
            messages,
            receiver: Mutex::new(Some(receiver)),
            runtime: tokio::runtime::Handle::current(),
            crossing: AtomicBool::new(false),
        });
        let running = hub.clone();
        let task = tokio::spawn(async move {
            let slots = Arc::new(tokio::sync::Semaphore::new(4));
            let mut clients = tokio::task::JoinSet::new();
            let mut poll = tokio::time::interval(std::time::Duration::from_millis(250));
            let mut count = None;
            loop {
                tokio::select! {
                    accepted = accept_connection(|| listener.accept()) => {
                        let (stream, address) = accepted;
                        let source = address.ip().to_canonical();
                        if !lock(&running.active).values().any(|(ip, _)| ip.to_canonical() == source) { continue; }
                        let Ok(slot) = slots.clone().try_acquire_owned() else { continue };
                        let hub = running.clone();
                        clients.spawn(async move {
                            let _slot = slot;
                            if let Err(error) = hub.serve(stream).await { tracing::warn!(error = ?error, "copied-file request failed"); }
                        });
                    }
                    _ = clients.join_next(), if !clients.is_empty() => {},
                    _ = poll.tick() => {
                        let hub = running.clone();
                        match tokio::task::spawn_blocking(move || hub.poll(&mut count)).await {
                            Ok(next) => count = next,
                            Err(error) => tracing::warn!(?error, "copied-file clipboard check failed"),
                        }
                    }
                }
            }
        });
        Ok((hub, task))
    }

    pub fn take_messages(&self) -> mpsc::Receiver<Message> {
        lock(&self.receiver).take().unwrap_or_else(|| mpsc::channel(1).1)
    }

    pub fn put_messages(&self, receiver: mpsc::Receiver<Message>) {
        *lock(&self.receiver) = Some(receiver);
    }

    pub fn crossing(&self) {
        self.crossing.store(true, Ordering::Release);
    }

    pub fn offered_to(&self, peer: PublicKey, id: OfferId) -> bool {
        lock(&self.local)
            .as_ref()
            .is_some_and(|local| local.snapshot.offer.id == id && local.leases.offered_to(peer))
    }

    pub fn join(self: &Arc<Self>, peer: PublicKey, ip: IpAddr) -> ActivePeer {
        let registration = self.registration.fetch_add(1, Ordering::Relaxed);
        lock(&self.active).insert(peer, (ip, registration));
        ActivePeer(self.clone(), peer, registration)
    }

    fn poll(self: &Arc<Self>, count: &mut Option<i64>) -> Option<i64> {
        let now = Instant::now();
        if let Some(local) = lock(&self.local).as_mut() {
            local.leases.expire(now);
        }
        if lock(&self.local).as_ref().is_some_and(|l| l.leases.empty()) {
            lock(&self.local).take();
        }
        #[cfg(all(target_os = "macos", not(test)))]
        {
            if lock(&self.active).is_empty() && lock(&self.remote).is_none() {
                return *count;
            }
            let current = crate::macos::file_pasteboard::count();
            let remote = lock(&self.remote).clone();
            if let Some(remote) = remote {
                let changed = !crate::macos::file_pasteboard::holds(remote.offer.id);
                let expired = remote.active.load(Ordering::Acquire) == 0 && *lock(&remote.until) <= now;
                if changed || expired || !*self.enabled.borrow() || !lock(&self.active).contains_key(&remote.peer) {
                    if self.remove_remote(&remote) && !changed {
                        crate::macos::file_pasteboard::clear(remote.offer.id);
                    }
                    let _ = self.messages.try_send(Message::FilesRelease { offer: remote.offer.id });
                }
                return Some(current);
            }
            let crossing = self.crossing.swap(false, Ordering::AcqRel);
            if *count != Some(current) || crossing {
                *count = Some(current);
                lock(&self.local).take();
                if crossing && *self.enabled.borrow() {
                    match crate::macos::file_pasteboard::copied_paths().and_then(|paths| self.offer(paths)) {
                        Ok(()) => {}
                        Err(error) => {
                            tracing::warn!(error = ?error, "copied files could not be offered");
                            crate::macos::file_pasteboard::offer_failed(format!("{error:#}"));
                        }
                    }
                }
            }
            Some(current)
        }
        #[cfg(any(not(target_os = "macos"), test))]
        {
            *count
        }
    }

    pub fn offer(&self, paths: Vec<PathBuf>) -> Result<()> {
        if paths.is_empty() || lock(&self.active).is_empty() {
            return Ok(());
        }
        let snapshot = Arc::new(snapshot(paths, self.port)?);
        let offer = snapshot.offer.clone();
        let peers = lock(&self.active).keys().copied().collect::<Vec<_>>();
        *lock(&self.local) = Some(Local {
            leases: Leases::new(offer.clone(), peers, Instant::now()),
            snapshot,
        });
        self.messages
            .try_send(Message::FilesOffer { offer })
            .context("file-offer queue is full")?;
        Ok(())
    }

    pub fn release(&self, peer: PublicKey, offer: OfferId) {
        if let Some(local) = lock(&self.local).as_mut()
            && local.snapshot.offer.id == offer
        {
            local.leases.release(peer);
        }
    }

    fn remove_remote(&self, expected: &Arc<Remote>) -> bool {
        let mut current = lock(&self.remote);
        if current.as_ref().is_some_and(|remote| Arc::ptr_eq(remote, expected)) {
            current.take();
            true
        } else {
            false
        }
    }

    fn touch(&self, peer: PublicKey, offer: OfferId, now: Instant) {
        if let Some(local) = lock(&self.local).as_mut()
            && local.snapshot.offer.id == offer
        {
            local.leases.touch(peer, now);
        }
    }

    pub fn accept(self: &Arc<Self>, peer: PublicKey, offer: Offer) -> Result<()> {
        offer.validate()?;
        ensure!(
            lock(&self.active).contains_key(&peer),
            "file offer from a disconnected peer"
        );
        if !*self.enabled.borrow() {
            return Ok(());
        }
        lock(&self.local).take();
        if let Some(old) = lock(&self.remote).take() {
            let _ = self.messages.try_send(Message::FilesRelease { offer: old.offer.id });
        }
        let remote = Arc::new(Remote {
            peer,
            offer,
            until: Mutex::new(Instant::now() + files::TTL),
            active: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        });
        #[cfg(all(target_os = "macos", not(test)))]
        crate::macos::file_pasteboard::install(self.clone(), remote.clone())?;
        *lock(&self.remote) = Some(remote);
        Ok(())
    }

    fn authenticate(&self, channel: &Channel<TcpStream>, expected: Option<PublicKey>) -> Result<()> {
        self.authenticate_cached(channel, expected, &files::TrustRefresh::default())
    }

    fn authenticate_cached(
        &self,
        channel: &Channel<TcpStream>,
        expected: Option<PublicKey>,
        refresh: &files::TrustRefresh,
    ) -> Result<()> {
        let key = channel.remote_key();
        ensure!(
            expected.is_none_or(|p| p == key),
            "bulk connection changed peer identity"
        );
        ensure!(channel.files_capable(), "file capability was not negotiated");
        ensure!(
            *self.enabled.borrow() && lock(&self.active).contains_key(&key),
            "file sharing is not active for this peer"
        );
        refresh.check(Instant::now(), || {
            let pinned = self
                .peers
                .trusted(&key, crate::trust::now())?
                .context("file requester is not trusted")?;
            ensure!(
                pinned.signing.is_some() && pinned.signing == channel.remote_device(),
                "bulk connection changed device identity"
            );
            Ok(())
        })
    }

    async fn serve(self: &Arc<Self>, stream: TcpStream) -> Result<()> {
        stream.set_nodelay(true)?;
        let hub = self.clone();
        let (mut channel, peer) = tokio::time::timeout(HANDSHAKE_TIMEOUT, async move {
            let mut channel = Channel::respond(stream, &hub.identity).await?;
            channel.authenticate_device(&hub.signer).await?;
            hub.authenticate(&channel, None)?;
            let peer = channel.remote_key();
            Ok::<_, anyhow::Error>((channel, peer))
        })
        .await
        .context("bulk device authentication timed out")??;
        let request = tokio::time::timeout(TIMEOUT, channel.recv()).await??;
        let Message::FilesRequest { offer, item } = request else {
            bail!("bulk channel accepts only offered-file requests")
        };
        let snapshot = {
            let mut local = lock(&self.local);
            let local = local.as_mut().context("no file offer is active")?;
            local.leases.acquire(peer, offer, item, Instant::now())?;
            local.snapshot.clone()
        };
        let hub = self.clone();
        tokio::task::spawn_blocking(move || {
            let result = send_item(&hub, &mut channel, peer, offer, &snapshot.roots[usize::from(item)]);
            if let Some(local) = lock(&hub.local).as_mut()
                && local.snapshot.offer.id == offer
            {
                local.leases.finish(peer, Instant::now());
            }
            if let Err(error) = &result {
                let _ = send(
                    &hub.runtime,
                    &mut channel,
                    Part::Failed {
                        reason: error.to_string().chars().take(512).collect(),
                    },
                );
            }
            result
        })
        .await?
    }

    /// Called on the native clipboard worker, never an input callback.
    pub fn fetch(
        &self,
        remote: &Remote,
        item: u16,
        destination: &Path,
        progress: &Progress,
        observe: impl Fn(u64) -> bool,
    ) -> Result<()> {
        let _entered = self.runtime.enter();
        ensure!(
            *lock(&remote.until) > Instant::now(),
            "copied-file offer expired; copy the files again"
        );
        ensure!(
            !remote.cancelled.load(Ordering::Acquire),
            "file offer is no longer available"
        );
        let offered = remote
            .offer
            .items
            .get(usize::from(item))
            .context("file item was not offered")?;
        let ip = lock(&self.active)
            .get(&remote.peer)
            .context("the offering peer disconnected")?
            .0;
        remote.active.fetch_add(1, Ordering::AcqRel);
        let result = (|| {
            let mut channel = wait_checked(
                &self.runtime,
                async {
                    let stream = TcpStream::connect(SocketAddr::new(ip, remote.offer.port)).await?;
                    stream.set_nodelay(true)?;
                    let mut channel = Channel::initiate(stream, &self.identity).await?;
                    channel.authenticate_device(&self.signer).await?;
                    self.authenticate(&channel, Some(remote.peer))?;
                    channel
                        .send(&Message::FilesRequest {
                            offer: remote.offer.id,
                            item,
                        })
                        .await?;
                    Ok::<_, anyhow::Error>(channel)
                },
                &|| {
                    ensure!(
                        !progress.cancelled.load(Ordering::Acquire) && !observe(progress.done.load(Ordering::Relaxed)),
                        "file transfer cancelled"
                    );
                    ensure!(
                        !remote.cancelled.load(Ordering::Acquire)
                            && *self.enabled.borrow()
                            && lock(&self.active).contains_key(&remote.peer),
                        "file sharing ended"
                    );
                    Ok(())
                },
            )?;
            let device = channel.remote_device();
            let refresh = files::TrustRefresh::default();
            receive_item(&self.runtime, &mut channel, offered, destination, progress, || {
                ensure!(
                    !observe(progress.done.load(Ordering::Relaxed)),
                    "file transfer cancelled"
                );
                ensure!(
                    !remote.cancelled.load(Ordering::Acquire)
                        && *self.enabled.borrow()
                        && lock(&self.active).contains_key(&remote.peer),
                    "file sharing ended"
                );
                refresh.check(Instant::now(), || {
                    let trusted = self
                        .peers
                        .trusted(&remote.peer, crate::trust::now())?
                        .context("file-sharing trust ended")?;
                    ensure!(
                        trusted.signing == device && device.is_some(),
                        "file-sharing device identity changed"
                    );
                    Ok(())
                })?;
                *lock(&remote.until) = Instant::now() + files::TTL;
                Ok(())
            })
        })();
        remote.active.fetch_sub(1, Ordering::AcqRel);
        *lock(&remote.until) = Instant::now() + files::TTL;
        result
    }
}

pub struct ActivePeer(Arc<Hub>, PublicKey, u64);
impl Drop for ActivePeer {
    fn drop(&mut self) {
        let mut active = lock(&self.0.active);
        if active
            .get(&self.1)
            .is_none_or(|(_, registration)| *registration != self.2)
        {
            return;
        }
        active.remove(&self.1);
        drop(active);
        if let Some(local) = lock(&self.0.local).as_mut() {
            local.leases.release(self.1);
        }
        if let Some(remote) = lock(&self.0.remote).as_ref().filter(|r| r.peer == self.1) {
            remote.cancelled.store(true, Ordering::Release);
        }
    }
}

fn open_root(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?)
}

fn snapshot(paths: Vec<PathBuf>, port: u16) -> Result<Snapshot> {
    ensure!(paths.len() <= files::MAX_ITEMS, "too many copied files");
    let mut id = [0; 16];
    getrandom::fill(&mut id)?;
    let mut roots = Vec::new();
    let mut items = Vec::new();
    let mut total = 0;
    let mut attributes = 0;
    let mut count = 0;
    for path in paths {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .context("copied file name is not UTF-8")?
            .to_owned();
        files::safe_name(&name)?;
        let file = open_root(&path)?;
        let mut entries = Vec::new();
        walk(&path, &path, &mut entries, &mut total, &mut attributes, &mut count)?;
        ensure!(count <= files::MAX_ENTRIES, "too many copied file entries");
        files::validate_links(
            &entries
                .iter()
                .filter_map(|entry| match &entry.kind {
                    Kind::Symlink { target } => Some((entry.path.clone(), target.clone())),
                    _ => None,
                })
                .collect(),
        )?;
        let bytes = entries
            .iter()
            .filter(|e| matches!(e.kind, Kind::File))
            .map(|e| e.len)
            .sum();
        items.push(Item {
            name,
            directory: file.metadata()?.is_dir(),
            bytes,
        });
        roots.push(Root { file, entries });
    }
    let offer = Offer { id, port, items };
    offer.validate()?;
    Ok(Snapshot { offer, roots })
}

fn walk(
    root: &Path,
    path: &Path,
    entries: &mut Vec<Entry>,
    total: &mut u64,
    attribute_total: &mut u64,
    count: &mut usize,
) -> Result<()> {
    ensure!(*count < files::MAX_ENTRIES, "copied folder contains too many entries");
    *count += 1;
    let relative = path
        .strip_prefix(root)?
        .to_str()
        .context("copied path is not UTF-8")?
        .to_owned();
    files::safe_relative(&relative)?;
    let meta = fs::symlink_metadata(path)?;
    let kind = if meta.is_file() {
        Kind::File
    } else if meta.is_dir() {
        Kind::Directory
    } else if meta.file_type().is_symlink() {
        let target = fs::read_link(path)?
            .to_str()
            .context("symbolic link is not UTF-8")?
            .to_owned();
        files::safe_link(&relative, &target)?;
        Kind::Symlink { target }
    } else {
        bail!("copied folders cannot contain special files")
    };
    let len = if matches!(kind, Kind::File) { meta.len() } else { 0 };
    *total = total.checked_add(len).context("copied-file size overflow")?;
    ensure!(*total <= files::MAX_BYTES, "copied files exceed the 4 GiB limit");
    let mut attributes = Vec::new();
    if matches!(kind, Kind::Symlink { .. }) {
        let mut bytes = 0;
        for name in xattr::list(path)? {
            ensure!(
                attributes.len() < files::MAX_ATTRIBUTES,
                "too many symbolic link attributes"
            );
            let value = xattr::get(path, &name)?.context("symbolic link attributes changed")?;
            bytes += value.len() as u64;
            *attribute_total += value.len() as u64;
            ensure!(
                *attribute_total <= files::MAX_ATTRIBUTE_BYTES,
                "copied symbolic link metadata exceeds 32 MiB"
            );
            ensure!(
                bytes <= files::MAX_ATTRIBUTE_BYTES,
                "oversized symbolic link attributes"
            );
            attributes.push((
                name.to_str()
                    .context("symbolic link attribute name is not UTF-8")?
                    .to_owned(),
                value,
            ));
        }
    }
    entries.push(Entry {
        path: relative,
        kind,
        len,
        mode: meta.mode() & 0o777,
        dev: meta.dev(),
        ino: meta.ino(),
        attributes,
    });
    if meta.is_dir() {
        let mut children = bounded_children(path, files::MAX_ENTRIES - *count)?;
        children.sort_by_key(|e| e.file_name());
        for child in children {
            walk(root, &child.path(), entries, total, attribute_total, count)?;
        }
    }
    Ok(())
}

fn bounded_children(path: &Path, remaining: usize) -> Result<Vec<fs::DirEntry>> {
    let mut children = Vec::new();
    for child in fs::read_dir(path)? {
        ensure!(children.len() < remaining, "copied folder contains too many entries");
        children.push(child?);
    }
    Ok(children)
}

fn opened(root: &File, entry: &Entry) -> Result<File> {
    let mut file = root.try_clone()?;
    for component in Path::new(&entry.path).components() {
        let name = CString::new(component.as_os_str().as_encoded_bytes())?;
        // SAFETY: the parent descriptor is owned; openat returns a new descriptor.
        // Every component refuses symlinks, including directory substitutions.
        let fd = unsafe {
            libc::openat(
                file.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        ensure!(
            fd >= 0,
            "copied file changed or cannot be read: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: successful openat transferred ownership of this descriptor.
        file = unsafe { File::from_raw_fd(fd) };
    }
    let meta = file.metadata()?;
    ensure!(
        meta.dev() == entry.dev
            && meta.ino() == entry.ino
            && (matches!(entry.kind, Kind::Directory) || meta.len() == entry.len),
        "copied file changed; copy it again"
    );
    Ok(file)
}

fn send(runtime: &tokio::runtime::Handle, channel: &mut Channel<TcpStream>, part: Part) -> Result<()> {
    runtime.block_on(tokio::time::timeout(
        TIMEOUT,
        channel.send(&Message::FilesPart { part }),
    ))??;
    Ok(())
}

fn wait_checked<T>(
    runtime: &tokio::runtime::Handle,
    future: impl std::future::Future<Output = Result<T>>,
    check: &impl Fn() -> Result<()>,
) -> Result<T> {
    runtime.block_on(async {
        let future = tokio::time::timeout(TIMEOUT, future);
        tokio::pin!(future);
        let mut every = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                result = &mut future => return result.context("file transfer timed out")?,
                _ = every.tick() => check()?,
            }
        }
    })
}

fn recv_checked(
    runtime: &tokio::runtime::Handle,
    channel: &mut Channel<TcpStream>,
    check: &impl Fn() -> Result<()>,
) -> Result<Part> {
    let message = wait_checked(runtime, async { Ok(channel.recv().await?) }, check)?;
    match message {
        Message::FilesPart {
            part: Part::Failed { reason },
        } => bail!("peer could not send copied file: {reason}"),
        Message::FilesPart { part } => Ok(part),
        _ => bail!("unexpected message on file-transfer connection"),
    }
}

fn send_data(
    hub: &Hub,
    channel: &mut Channel<TcpStream>,
    peer: PublicKey,
    offer: OfferId,
    mut reader: impl Read,
    refresh: &files::TrustRefresh,
) -> Result<()> {
    let mut buffer = vec![0; files::CHUNK];
    loop {
        hub.authenticate_cached(channel, Some(peer), refresh)?;
        let length = reader.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        send(
            &hub.runtime,
            channel,
            Part::Data {
                bytes: buffer[..length].to_vec(),
            },
        )?;
        hub.touch(peer, offer, Instant::now());
    }
    send(&hub.runtime, channel, Part::DataEnd)
}

fn send_item(hub: &Hub, channel: &mut Channel<TcpStream>, peer: PublicKey, offer: OfferId, root: &Root) -> Result<()> {
    let refresh = files::TrustRefresh::default();
    let mut attribute_bytes = 0u64;
    for entry in &root.entries {
        hub.authenticate_cached(channel, Some(peer), &refresh)?;
        if matches!(entry.kind, Kind::Symlink { .. }) {
            send(
                &hub.runtime,
                channel,
                Part::Entry {
                    path: entry.path.clone(),
                    kind: entry.kind.clone(),
                    len: 0,
                    mode: entry.mode,
                    compressed: false,
                },
            )?;
            send(&hub.runtime, channel, Part::DataEnd)?;
            for (name, bytes) in &entry.attributes {
                attribute_bytes += bytes.len() as u64;
                ensure!(
                    attribute_bytes <= files::MAX_ATTRIBUTE_BYTES,
                    "file metadata exceeds 32 MiB"
                );
                send(
                    &hub.runtime,
                    channel,
                    Part::Attribute {
                        name: name.clone(),
                        len: bytes.len() as u64,
                    },
                )?;
                send_data(hub, channel, peer, offer, bytes.as_slice(), &refresh)?;
            }
        } else {
            let file = opened(&root.file, entry)?;
            let mut sample = vec![0; (entry.len as usize).min(64 << 10)];
            if matches!(entry.kind, Kind::File) {
                file.read_exact_at(&mut sample, 0)?;
            }
            let level = if entry.len > 64 << 20 { 1 } else { 3 };
            let compressed = !sample.is_empty() && zstd::bulk::compress(&sample, level)?.len() < sample.len();
            send(
                &hub.runtime,
                channel,
                Part::Entry {
                    path: entry.path.clone(),
                    kind: entry.kind.clone(),
                    len: entry.len,
                    mode: entry.mode,
                    compressed,
                },
            )?;
            if matches!(entry.kind, Kind::File) {
                let reader = ExactReader {
                    file: file.try_clone()?,
                    left: entry.len,
                    offset: 0,
                };
                if compressed {
                    send_data(
                        hub,
                        channel,
                        peer,
                        offer,
                        zstd::stream::read::Encoder::new(reader, level)?,
                        &refresh,
                    )?;
                } else {
                    send_data(hub, channel, peer, offer, reader, &refresh)?;
                }
            } else {
                send(&hub.runtime, channel, Part::DataEnd)?;
            }
            let attributes = file.list_xattr()?.collect::<Vec<_>>();
            ensure!(
                attributes.len() <= files::MAX_ATTRIBUTES,
                "too many extended attributes"
            );
            for name in attributes {
                let name = name
                    .to_str()
                    .context("extended attribute name is not UTF-8")?
                    .to_owned();
                let bytes = file
                    .get_xattr(&name)?
                    .context("extended attribute changed during transfer")?;
                attribute_bytes += bytes.len() as u64;
                ensure!(
                    attribute_bytes <= files::MAX_ATTRIBUTE_BYTES,
                    "extended attributes exceed the 32 MiB limit"
                );
                send(
                    &hub.runtime,
                    channel,
                    Part::Attribute {
                        name,
                        len: bytes.len() as u64,
                    },
                )?;
                send_data(hub, channel, peer, offer, bytes.as_slice(), &refresh)?;
            }
        }
        send(&hub.runtime, channel, Part::EntryEnd)?;
    }
    send(&hub.runtime, channel, Part::Finished)
}

struct ExactReader {
    file: File,
    left: u64,
    offset: u64,
}
impl Read for ExactReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.left == 0 {
            let mut extra = [0];
            if self.file.read_at(&mut extra, self.offset)? != 0 {
                return Err(std::io::Error::other("copied file grew during transfer"));
            }
            return Ok(0);
        }
        let length = buffer.len().min(self.left as usize);
        let n = self.file.read_at(&mut buffer[..length], self.offset)?;
        if n == 0 {
            return Err(std::io::Error::other("copied file shrank during transfer"));
        }
        self.left -= n as u64;
        self.offset += n as u64;
        Ok(n)
    }
}

struct DataReader<'a, F> {
    runtime: &'a tokio::runtime::Handle,
    channel: &'a mut Channel<TcpStream>,
    check: &'a F,
    buffer: std::io::Cursor<Vec<u8>>,
    ended: bool,
    wire: u64,
}
impl<F: Fn() -> Result<()>> Read for DataReader<'_, F> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        (self.check)().map_err(std::io::Error::other)?;
        let n = self.buffer.read(out)?;
        if n != 0 || self.ended {
            return Ok(n);
        }
        match recv_checked(self.runtime, self.channel, self.check).map_err(std::io::Error::other)? {
            Part::Data { bytes } if !bytes.is_empty() && bytes.len() <= files::CHUNK => {
                self.wire += bytes.len() as u64;
                if self.wire > files::MAX_BYTES * 2 {
                    return Err(std::io::Error::other("oversized file stream"));
                }
                self.buffer = std::io::Cursor::new(bytes);
                self.buffer.read(out)
            }
            Part::DataEnd => {
                self.ended = true;
                Ok(0)
            }
            _ => Err(std::io::Error::other("unexpected file data")),
        }
    }
}

fn receive_data(
    runtime: &tokio::runtime::Handle,
    channel: &mut Channel<TcpStream>,
    writer: &mut impl Write,
    len: u64,
    compressed: bool,
    progress: Option<&Progress>,
    check: &impl Fn() -> Result<()>,
) -> Result<()> {
    let reader = DataReader {
        runtime,
        channel,
        check,
        buffer: std::io::Cursor::new(Vec::new()),
        ended: false,
        wire: 0,
    };
    let mut reader: Box<dyn Read + '_> = if compressed {
        let mut decoder = zstd::stream::read::Decoder::new(reader)?;
        decoder.window_log_max(23)?;
        Box::new(decoder)
    } else {
        Box::new(reader)
    };
    let mut done = 0u64;
    let mut buffer = vec![0; files::CHUNK];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        done = done.checked_add(n as u64).context("file size overflow")?;
        ensure!(done <= len, "received file exceeds its declared size");
        writer.write_all(&buffer[..n])?;
        if let Some(progress) = progress {
            progress.done.fetch_add(n as u64, Ordering::Relaxed);
        }
    }
    ensure!(done == len, "received file is shorter than declared");
    Ok(())
}

struct Temporary(PathBuf);
impl Drop for Temporary {
    fn drop(&mut self) {
        make_removable(&self.0);
        if self.0.is_dir() {
            let _ = fs::remove_dir_all(&self.0);
        } else {
            let _ = fs::remove_file(&self.0);
        }
    }
}

fn make_removable(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if let Ok(children) = fs::read_dir(path) {
            for child in children.flatten() {
                make_removable(&child.path());
            }
        }
    }
}

fn validate_staged_links(root: &Path, links: &BTreeMap<String, String>) -> Result<()> {
    // Resolve through the receiving filesystem itself: case and Unicode aliases
    // need not have the same manifest spelling. Dangling internal links remain
    // valid, but every parent traversal is bounded by the offered root.
    for (path, target) in links {
        let mut current = root.join(path).parent().context("link has no parent")?.to_path_buf();
        let mut pending: std::collections::VecDeque<_> = Path::new(target)
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect();
        let mut expansions = 0;
        while let Some(part) = pending.pop_front() {
            if part == "." {
                continue;
            }
            if part == ".." {
                ensure!(
                    current != root && current.pop(),
                    "symbolic link escapes its offered folder"
                );
                continue;
            }
            ensure!(
                Path::new(&part)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "invalid symbolic link component"
            );
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    expansions += 1;
                    ensure!(
                        expansions <= 40,
                        "symbolic link graph contains a cycle or too many links"
                    );
                    let next = fs::read_link(&current)?;
                    current.pop();
                    for component in next.components().rev() {
                        pending.push_front(component.as_os_str().to_owned());
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        ensure!(current.starts_with(root), "symbolic link escapes its offered folder");
    }
    Ok(())
}

fn receive_item(
    runtime: &tokio::runtime::Handle,
    channel: &mut Channel<TcpStream>,
    item: &Item,
    destination: &Path,
    progress: &Progress,
    check: impl Fn() -> Result<()>,
) -> Result<()> {
    let parent = destination.parent().context("file destination has no parent")?;
    ensure!(parent.is_dir(), "file destination folder does not exist");
    let mut id = [0; 16];
    getrandom::fill(&mut id)?;
    let temporary = Temporary(parent.join(format!(".daisy-{:032x}.part", u128::from_ne_bytes(id))));
    let mut seen = BTreeSet::new();
    let mut directories = Vec::new();
    let mut symlinks = Vec::new();
    let mut symlink_targets = BTreeMap::new();
    let mut link_attributes = BTreeMap::<PathBuf, Vec<(String, Vec<u8>)>>::new();
    let mut retained_attributes = 0u64;
    let mut attribute_bytes = 0u64;
    let mut total = 0u64;
    let check = || {
        ensure!(!progress.cancelled.load(Ordering::Acquire), "file transfer cancelled");
        check()
    };
    loop {
        check()?;
        let part = recv_checked(runtime, channel, &check)?;
        let (path, kind, len, mode, compressed) = match part {
            Part::Entry {
                path,
                kind,
                len,
                mode,
                compressed,
            } => (path, kind, len, mode, compressed),
            Part::Finished => break,
            _ => bail!("unexpected file entry"),
        };
        files::safe_relative(&path)?;
        ensure!(
            seen.len() < files::MAX_ENTRIES && seen.insert(path.clone()),
            "duplicate or excess file entries"
        );
        let mut count = progress.entries.load(Ordering::Acquire);
        loop {
            ensure!(
                count < files::MAX_ENTRIES as u64,
                "copied files contain too many entries"
            );
            match progress
                .entries
                .compare_exchange_weak(count, count + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        if seen.len() == 1 {
            ensure!(
                path.is_empty() && item.directory == matches!(kind, Kind::Directory),
                "file root does not match the offered item"
            );
        } else {
            ensure!(
                item.directory && !path.is_empty(),
                "extra entries in a single-file offer"
            );
        }
        let output = temporary.0.join(&path);
        let output = if path.is_empty() { temporary.0.clone() } else { output };
        ensure!(
            path.is_empty() || output.parent().is_some_and(Path::is_dir),
            "file parent was not declared"
        );
        let mut file = match kind {
            Kind::File => {
                total = total.checked_add(len).context("file size overflow")?;
                ensure!(
                    total <= item.bytes && total <= files::MAX_BYTES,
                    "file data exceeds the offer"
                );
                Some(
                    OpenOptions::new()
                        .write(true)
                        .read(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&output)?,
                )
            }
            Kind::Directory => {
                ensure!(len == 0 && !compressed, "invalid folder data");
                fs::create_dir(&output)?;
                fs::set_permissions(&output, fs::Permissions::from_mode(0o700))?;
                directories.push((output.clone(), mode & 0o777));
                Some(File::open(&output)?)
            }
            Kind::Symlink { target } => {
                symlink_targets.insert(path.clone(), target.clone());
                ensure!(
                    len == 0 && !compressed && !path.is_empty(),
                    "invalid symbolic link data"
                );
                files::safe_link(&path, &target)?;
                symlinks.push((output.clone(), target));
                None
            }
        };
        if let Some(file) = file.as_mut() {
            if len > 0 || compressed {
                receive_data(runtime, channel, file, len, compressed, Some(progress), &check)?;
            } else {
                ensure!(
                    matches!(recv_checked(runtime, channel, &check)?, Part::DataEnd),
                    "unexpected empty-file data"
                );
            }
        } else {
            ensure!(
                matches!(recv_checked(runtime, channel, &check)?, Part::DataEnd),
                "unexpected symbolic link data"
            );
        }
        let mut attrs = BTreeSet::new();
        loop {
            match recv_checked(runtime, channel, &check)? {
                Part::EntryEnd => break,
                Part::Attribute { name, len } => {
                    ensure!(
                        name.len() <= 255
                            && !name.is_empty()
                            && !name.contains('\0')
                            && attrs.len() < files::MAX_ATTRIBUTES
                            && attrs.insert(name.clone()),
                        "invalid or duplicate extended attribute"
                    );
                    attribute_bytes = attribute_bytes
                        .checked_add(len)
                        .context("extended attribute size overflow")?;
                    ensure!(
                        attribute_bytes <= files::MAX_ATTRIBUTE_BYTES,
                        "oversized extended attributes"
                    );
                    let mut bytes = Vec::new();
                    receive_data(runtime, channel, &mut bytes, len, false, None, &check)?;
                    if let Some(file) = &file {
                        file.set_xattr(&name, &bytes)?;
                    } else {
                        retained_attributes += len;
                        ensure!(
                            retained_attributes <= files::MAX_ATTRIBUTE_BYTES,
                            "too much symbolic link metadata"
                        );
                        link_attributes.entry(output.clone()).or_default().push((name, bytes));
                    }
                }
                _ => bail!("unexpected file attribute"),
            }
        }
        if let Some(file) = file
            && file.metadata()?.is_file()
        {
            file.set_permissions(fs::Permissions::from_mode(mode & 0o777))?;
            file.sync_all()?;
        }
    }
    ensure!(!seen.is_empty() && total == item.bytes, "incomplete offered file");
    files::validate_links(&symlink_targets)?;
    for (path, target) in symlinks {
        std::os::unix::fs::symlink(target, &path)?;
        for (name, bytes) in link_attributes.remove(&path).unwrap_or_default() {
            xattr::set(&path, name, &bytes)?;
        }
    }
    validate_staged_links(&temporary.0, &symlink_targets)?;
    for (path, mode) in directories.into_iter().rev() {
        let file = File::open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(mode))?;
        file.sync_all()?;
    }
    check()?;
    publish(&temporary.0, destination)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn publish(from: &Path, to: &Path) -> Result<()> {
    let from = CString::new(from.as_os_str().as_encoded_bytes())?;
    let to = CString::new(to.as_os_str().as_encoded_bytes())?;
    #[cfg(target_os = "macos")]
    // SAFETY: both strings are NUL-terminated. RENAME_EXCL never overwrites.
    let result = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(target_os = "linux")]
    // SAFETY: both strings are NUL-terminated; paths are relative to AT_FDCWD.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    ensure!(
        result == 0,
        "file destination could not be published: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::Policy;

    struct Pair {
        _homes: [tempfile::TempDir; 2],
        source: Arc<Hub>,
        destination: Arc<Hub>,
        _peers: [ActivePeer; 2],
        tasks: [tokio::task::JoinHandle<()>; 2],
    }

    impl Drop for Pair {
        fn drop(&mut self) {
            for task in &self.tasks {
                task.abort();
            }
        }
    }

    fn pair() -> Pair {
        pair_at(IpAddr::from([127, 0, 0, 1]))
    }

    fn pair_at(ip: IpAddr) -> Pair {
        let homes = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        let sa = crate::device::Signer::generate().unwrap();
        let sb = crate::device::Signer::generate().unwrap();
        let pa = crate::peers::PeerStore::open(homes[0].path()).unwrap();
        let pb = crate::peers::PeerStore::open(homes[1].path()).unwrap();
        pa.pin_device(
            b.public_key(),
            sb.public(),
            "Peer B",
            Policy::Forever,
            crate::trust::now(),
        )
        .unwrap();
        pb.pin_device(
            a.public_key(),
            sa.public(),
            "Peer A",
            Policy::Forever,
            crate::trust::now(),
        )
        .unwrap();
        let (_on, enabled) = watch::channel(true);
        let (source, ta) = Hub::start(a, sa, pa, enabled.clone()).unwrap();
        let (destination, tb) = Hub::start(b, sb, pb, enabled).unwrap();
        let ga = source.join(destination.identity.public_key(), ip);
        let gb = destination.join(source.identity.public_key(), ip);
        Pair {
            _homes: homes,
            source,
            destination,
            _peers: [ga, gb],
            tasks: [ta, tb],
        }
    }

    fn remote_offer(source: &Hub) -> Arc<Remote> {
        Arc::new(Remote {
            peer: source.identity.public_key(),
            offer: lock(&source.local).as_ref().unwrap().snapshot.offer.clone(),
            until: Mutex::new(Instant::now() + files::TTL),
            active: AtomicU64::new(0),
            cancelled: AtomicBool::new(false),
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn copied_files_transfer_over_ipv6() {
        let pair = pair_at(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let file = source.path().join("copied.txt");
        fs::write(&file, b"ipv6 file contents").unwrap();
        pair.source.offer(vec![file]).unwrap();
        let remote = remote_offer(&pair.source);
        let hub = pair.destination.clone();
        let output = destination.path().join("copied.txt");
        tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |_| false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read(destination.path().join("copied.txt")).unwrap(),
            b"ipv6 file contents"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn encrypted_file_transfers_support_repeated_and_concurrent_pastes() {
        let pair = pair();
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let contents = (0..1024 * 1024).map(|n| (n % 251) as u8).collect::<Vec<_>>();
        let file = source.path().join("copied.bin");
        fs::write(&file, &contents).unwrap();
        pair.source.offer(vec![file]).unwrap();
        let remote = remote_offer(&pair.source);
        let mut pastes = Vec::new();
        for index in 0..2 {
            let hub = pair.destination.clone();
            let remote = remote.clone();
            let output = destination.path().join(format!("paste-{index}"));
            pastes.push(tokio::task::spawn_blocking(move || {
                hub.fetch(&remote, 0, &output, &Progress::default(), |_| false)
            }));
        }
        for paste in pastes {
            paste.await.unwrap().unwrap();
        }
        for index in 0..2 {
            assert_eq!(
                fs::read(destination.path().join(format!("paste-{index}"))).unwrap(),
                contents
            );
        }
        let hub = pair.destination.clone();
        let remote = remote.clone();
        let output = destination.path().join("paste-again");
        tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |_| false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(destination.path().join("paste-again")).unwrap(), contents);
    }

    #[tokio::test]
    async fn dropping_replaced_membership_keeps_the_current_link() {
        let pair = pair();
        let peer = pair.destination.identity.public_key();
        let old = pair.source.join(peer, IpAddr::from([127, 0, 0, 1]));
        let current = pair.source.join(peer, IpAddr::from([127, 0, 0, 2]));
        drop(old);
        assert_eq!(
            lock(&pair.source.active).get(&peer).unwrap().0,
            IpAddr::from([127, 0, 0, 2])
        );
        drop(current);
        assert!(!lock(&pair.source.active).contains_key(&peer));
    }

    #[tokio::test]
    async fn accepting_remote_files_supersedes_local_offer_and_stale_cleanup_cannot_remove_replacement() {
        let pair = pair();
        let input = tempfile::tempdir().unwrap();
        let file = input.path().join("copied");
        fs::write(&file, b"data").unwrap();
        pair.source.offer(vec![file]).unwrap();
        let old = remote_offer(&pair.source);
        let peer = pair.destination.identity.public_key();
        let mut incoming = old.offer.clone();
        incoming.id = [9; 16];
        pair.source.accept(peer, incoming.clone()).unwrap();
        assert!(!pair.source.offered_to(peer, old.offer.id));
        let examined = lock(&pair.source.remote).clone().unwrap();
        incoming.id = [10; 16];
        pair.source.accept(peer, incoming.clone()).unwrap();
        assert!(!pair.source.remove_remote(&examined));
        assert_eq!(lock(&pair.source.remote).as_ref().unwrap().offer.id, incoming.id);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn handshake_cancellation_does_not_wait_for_network_timeout() {
        let pair = pair();
        let input = tempfile::tempdir().unwrap();
        let output = tempfile::tempdir().unwrap();
        let file = input.path().join("copied");
        fs::write(&file, b"data").unwrap();
        pair.source.offer(vec![file]).unwrap();
        let mut remote = remote_offer(&pair.source);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        Arc::get_mut(&mut remote).unwrap().offer.port = listener.local_addr().unwrap().port();
        let (accepted, waiting) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
            let _ = accepted.send(());
            std::future::pending::<()>().await;
        });
        let progress = Arc::new(Progress::default());
        let cancellation = progress.clone();
        let hub = pair.destination.clone();
        let destination = output.path().join("must-not-exist");
        let mut receiving =
            tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &destination, &progress, |_| false));
        waiting.await.unwrap();
        cancellation.cancelled.store(true, Ordering::Release);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), &mut receiving).await;
        server.abort();
        assert!(
            result.is_ok(),
            "Cancel must interrupt the handshake without waiting 30 seconds"
        );
        assert!(format!("{:#}", result.unwrap().unwrap().unwrap_err()).contains("cancelled"));
        assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn incompressible_files_are_sent_without_a_compression_context() {
        let pair = pair();
        let input = tempfile::tempdir().unwrap();
        let file = input.path().join("random");
        let mut contents = vec![0; 64 << 10];
        getrandom::fill(&mut contents).unwrap();
        fs::write(&file, &contents).unwrap();
        pair.source.offer(vec![file]).unwrap();
        let remote = remote_offer(&pair.source);
        let stream = TcpStream::connect(("127.0.0.1", remote.offer.port)).await.unwrap();
        let mut channel = Channel::initiate(stream, &pair.destination.identity).await.unwrap();
        channel.authenticate_device(&pair.destination.signer).await.unwrap();
        channel
            .send(&Message::FilesRequest {
                offer: remote.offer.id,
                item: 0,
            })
            .await
            .unwrap();
        assert!(matches!(
            channel.recv().await.unwrap(),
            Message::FilesPart {
                part: Part::Entry { compressed: false, .. }
            }
        ));
        let mut received = Vec::new();
        loop {
            match channel.recv().await.unwrap() {
                Message::FilesPart {
                    part: Part::Data { bytes },
                } => received.extend(bytes),
                Message::FilesPart { part: Part::DataEnd } => break,
                other => panic!("unexpected file data: {other:?}"),
            }
        }
        assert_eq!(received, contents);
    }

    #[tokio::test]
    async fn old_transfer_activity_cannot_renew_a_new_offers_lease() {
        let pair = pair();
        let input = tempfile::tempdir().unwrap();
        let file = input.path().join("copied");
        fs::write(&file, b"data").unwrap();
        pair.source.offer(vec![file.clone()]).unwrap();
        let old = remote_offer(&pair.source);
        pair.source.offer(vec![file]).unwrap();
        let current = remote_offer(&pair.source);
        let now = Instant::now();
        let peer = pair.destination.identity.public_key();
        pair.source.touch(peer, old.offer.id, now + files::TTL * 2);
        assert!(
            lock(&pair.source.local)
                .as_mut()
                .unwrap()
                .leases
                .acquire(peer, current.offer.id, 0, now + files::TTL * 2)
                .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bulk_requests_reject_unknown_released_expired_superseded_and_unoffered_items() {
        for case in 0..5 {
            let pair = pair();
            let input = tempfile::tempdir().unwrap();
            let output = tempfile::tempdir().unwrap();
            let file = input.path().join("copied");
            fs::write(&file, b"data").unwrap();
            pair.source.offer(vec![file.clone()]).unwrap();
            let mut remote = remote_offer(&pair.source);
            let peer = pair.destination.identity.public_key();
            let mut item = 0;
            match case {
                0 => Arc::get_mut(&mut remote).unwrap().offer.id = [42; 16],
                1 => pair.source.release(peer, remote.offer.id),
                2 => lock(&pair.source.local)
                    .as_mut()
                    .unwrap()
                    .leases
                    .expire(Instant::now() + files::TTL * 2),
                3 => pair.source.offer(vec![file]).unwrap(),
                _ => {
                    let remote = Arc::get_mut(&mut remote).unwrap();
                    remote.offer.items.push(remote.offer.items[0].clone());
                    item = 1;
                }
            }
            let hub = pair.destination.clone();
            let destination = output.path().join("must-not-exist");
            let result = tokio::task::spawn_blocking(move || {
                hub.fetch(&remote, item, &destination, &Progress::default(), |_| false)
            })
            .await
            .unwrap();
            assert!(result.is_err(), "case {case}");
            assert_eq!(fs::read_dir(output.path()).unwrap().count(), 0);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn folders_keep_contents_permissions_quarantine_and_extended_attributes() {
        let pair = pair();
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let folder = source.path().join("Copied folder");
        fs::create_dir(&folder).unwrap();
        fs::create_dir(folder.join("nested")).unwrap();
        let file = folder.join("nested/file");
        fs::write(&file, b"copied contents").unwrap();
        fs::write(folder.join("empty"), []).unwrap();
        std::os::unix::fs::symlink("nested/file", folder.join("link")).unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
        let attribute = if cfg!(target_os = "macos") {
            "com.apple.quarantine"
        } else {
            "user.quarantine"
        };
        let folder_attribute = if cfg!(target_os = "macos") {
            "dev.misfit.daisy.test"
        } else {
            "user.daisy.test"
        };
        xattr::set(&file, attribute, b"0081;00000000;Daisy;").unwrap();
        xattr::set(&folder, folder_attribute, b"folder metadata").unwrap();
        pair.source.offer(vec![folder]).unwrap();
        let remote = remote_offer(&pair.source);
        let hub = pair.destination.clone();
        let output = destination.path().join("Pasted folder");
        tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |_| false))
            .await
            .unwrap()
            .unwrap();
        let output = destination.path().join("Pasted folder");
        assert_eq!(fs::read(output.join("nested/file")).unwrap(), b"copied contents");
        assert_eq!(fs::metadata(output.join("empty")).unwrap().len(), 0);
        assert_eq!(fs::read_link(output.join("link")).unwrap(), Path::new("nested/file"));
        assert_eq!(fs::metadata(output.join("nested/file")).unwrap().mode() & 0o777, 0o640);
        assert_eq!(
            xattr::get(output.join("nested/file"), attribute).unwrap().unwrap(),
            b"0081;00000000;Daisy;"
        );
        assert_eq!(
            xattr::get(&output, folder_attribute).unwrap().unwrap(),
            b"folder metadata"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancellation_removes_partial_files_and_never_replaces_existing_destinations() {
        let pair = pair();
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let file = source.path().join("copied");
        fs::write(&file, vec![42; 1024 * 1024]).unwrap();
        pair.source.offer(vec![file]).unwrap();
        let remote = remote_offer(&pair.source);
        let hub = pair.destination.clone();
        let output = destination.path().join("cancelled");
        assert!(
            tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |done| done > 0))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
        let output = destination.path().join("existing");
        fs::write(&output, b"keep me").unwrap();
        let remote = remote_offer(&pair.source);
        let hub = pair.destination.clone();
        assert!(
            tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |_| false))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(fs::read(destination.path().join("existing")).unwrap(), b"keep me");
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_matching_noise_key_cannot_override_a_different_bulk_device_key() {
        let pair = pair();
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let file = source.path().join("copied");
        fs::write(&file, b"contents").unwrap();
        let impostor_signer = crate::device::Signer::generate().unwrap();
        let (impostor, task) = Hub::start(
            pair.source.identity.clone(),
            impostor_signer,
            pair.source.peers.clone(),
            pair.source.enabled.clone(),
        )
        .unwrap();
        let _peer = impostor.join(pair.destination.identity.public_key(), IpAddr::from([127, 0, 0, 1]));
        impostor.offer(vec![file]).unwrap();
        let remote = remote_offer(&impostor);
        let hub = pair.destination.clone();
        let output = destination.path().join("must-not-exist");
        let result =
            tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |_| false))
                .await
                .unwrap();
        task.abort();
        assert!(format!("{:#}", result.unwrap_err()).contains("device identity"));
        assert_eq!(fs::read_dir(destination.path()).unwrap().count(), 0);
    }

    #[test]
    fn source_reads_refuse_symlink_substitution_and_size_changes() {
        let source = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let folder = source.path().join("folder");
        fs::create_dir(&folder).unwrap();
        fs::create_dir(folder.join("nested")).unwrap();
        fs::write(folder.join("nested/file"), b"offered").unwrap();
        let snapshot = snapshot(vec![folder.clone()], 1234).unwrap();
        let entry = snapshot.roots[0]
            .entries
            .iter()
            .find(|e| e.path == "nested/file")
            .unwrap();
        fs::remove_dir_all(folder.join("nested")).unwrap();
        fs::write(outside.path().join("file"), b"private").unwrap();
        std::os::unix::fs::symlink(outside.path(), folder.join("nested")).unwrap();
        assert!(opened(&snapshot.roots[0].file, entry).is_err());
        let file = source.path().join("plain");
        fs::write(&file, b"short").unwrap();
        let snapshot = super::snapshot(vec![file.clone()], 1234).unwrap();
        fs::write(file, b"longer than offered").unwrap();
        assert!(opened(&snapshot.roots[0].file, &snapshot.roots[0].entries[0]).is_err());
    }

    async fn malicious_stream(parts: Vec<Part>, item: Item) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let sender = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let identity = Identity::generate().unwrap();
            let mut channel = Channel::respond(stream, &identity).await.unwrap();
            for part in parts {
                if channel.send(&Message::FilesPart { part }).await.is_err() {
                    break;
                }
            }
        });
        let identity = Identity::generate().unwrap();
        let stream = TcpStream::connect(address).await.unwrap();
        let mut channel = Channel::initiate(stream, &identity).await.unwrap();
        let destination = tempfile::tempdir().unwrap();
        let parent = destination.path().to_owned();
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            receive_item(
                &runtime,
                &mut channel,
                &item,
                &parent.join("pasted"),
                &Progress::default(),
                || Ok(()),
            )
        })
        .await
        .unwrap();
        sender.await.unwrap();
        assert!(result.is_err(), "malformed file stream was accepted");
        assert_eq!(
            fs::read_dir(destination.path()).unwrap().count(),
            0,
            "malformed stream left visible or temporary files"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn symlink_graph_stream_cannot_publish_an_escape() {
        let entry = |path: &str, kind| Part::Entry {
            path: path.into(),
            kind,
            len: 0,
            mode: 0o700,
            compressed: false,
        };
        malicious_stream(
            vec![
                entry("", Kind::Directory),
                Part::DataEnd,
                Part::EntryEnd,
                entry("a", Kind::Symlink { target: ".".into() }),
                Part::DataEnd,
                Part::EntryEnd,
                entry("b", Kind::Symlink { target: "a/..".into() }),
                Part::DataEnd,
                Part::EntryEnd,
                Part::Finished,
            ],
            Item {
                name: "folder".into(),
                directory: true,
                bytes: 0,
            },
        )
        .await;
        let source = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(".", source.path().join("a")).unwrap();
        std::os::unix::fs::symlink("a/..", source.path().join("b")).unwrap();
        assert!(snapshot(vec![source.path().to_owned()], 1234).is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_streams_cannot_escape_expand_past_declarations_or_publish_partial_files() {
        let item = Item {
            name: "file".into(),
            directory: false,
            bytes: 1,
        };
        let entry = |len, compressed| Part::Entry {
            path: "".into(),
            kind: Kind::File,
            len,
            mode: 0o600,
            compressed,
        };
        malicious_stream(
            vec![
                entry(1, false),
                Part::Data {
                    bytes: b"too much".to_vec(),
                },
                Part::DataEnd,
                Part::EntryEnd,
                Part::Finished,
            ],
            item.clone(),
        )
        .await;
        malicious_stream(
            vec![entry(1, false), Part::DataEnd, Part::EntryEnd, Part::Finished],
            item.clone(),
        )
        .await;
        malicious_stream(
            vec![
                entry(1, false),
                Part::Data { bytes: vec![42] },
                Part::DataEnd,
                Part::EntryEnd,
            ],
            item.clone(),
        )
        .await;
        malicious_stream(
            vec![
                entry(1, true),
                Part::Data {
                    bytes: zstd::bulk::compress(&vec![42; 1 << 20], 1).unwrap(),
                },
                Part::DataEnd,
                Part::EntryEnd,
                Part::Finished,
            ],
            item.clone(),
        )
        .await;
        let root = Part::Entry {
            path: "".into(),
            kind: Kind::Directory,
            len: 0,
            mode: 0o700,
            compressed: false,
        };
        let outside = Part::Entry {
            path: "../escaped".into(),
            kind: Kind::File,
            len: 1,
            mode: 0o600,
            compressed: false,
        };
        malicious_stream(
            vec![
                root,
                Part::DataEnd,
                Part::EntryEnd,
                outside,
                Part::Data { bytes: vec![42] },
                Part::DataEnd,
                Part::EntryEnd,
                Part::Finished,
            ],
            Item {
                directory: true,
                ..item
            },
        )
        .await;
    }
    #[test]
    fn source_directory_listing_stops_at_remaining_entry_budget() {
        let folder = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::write(folder.path().join(name), b"").unwrap();
        }
        assert!(bounded_children(folder.path(), 2).is_err());
        assert_eq!(bounded_children(folder.path(), 3).unwrap().len(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn receive_entry_budget_is_shared_across_offer_items() {
        let pair = pair();
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["a", "b"]
            .into_iter()
            .map(|n| {
                let p = source.path().join(n);
                fs::write(&p, b"content").unwrap();
                p
            })
            .collect();
        pair.source.offer(paths).unwrap();
        let remote = remote_offer(&pair.source);
        let hub = pair.destination.clone();
        let folder = destination.path().to_owned();
        tokio::task::spawn_blocking(move || {
            let progress = Progress::default();
            progress.entries.store(files::MAX_ENTRIES as u64 - 1, Ordering::Relaxed);
            hub.fetch(&remote, 0, &folder.join("a"), &progress, |_| false).unwrap();
            assert!(hub.fetch(&remote, 1, &folder.join("b"), &progress, |_| false).is_err());
            assert!(!folder.join("b").exists());
            assert_eq!(fs::read_dir(folder).unwrap().count(), 1);
        })
        .await
        .unwrap();
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn aliased_manifest_paths_cannot_publish_symbolic_links() {
        let entry = |path: &str, kind| Part::Entry {
            path: path.into(),
            kind,
            len: 0,
            mode: 0o700,
            compressed: false,
        };
        malicious_stream(
            vec![
                entry("", Kind::Directory),
                Part::DataEnd,
                Part::EntryEnd,
                entry("a", Kind::Directory),
                Part::DataEnd,
                Part::EntryEnd,
                entry("a//link", Kind::Symlink { target: ".".into() }),
                Part::DataEnd,
                Part::EntryEnd,
                entry(
                    "escape",
                    Kind::Symlink {
                        target: "a/link/../..".into(),
                    },
                ),
                Part::DataEnd,
                Part::EntryEnd,
                Part::Finished,
            ],
            Item {
                name: "folder".into(),
                directory: true,
                bytes: 0,
            },
        )
        .await;
    }

    #[test]
    fn staged_link_validation_uses_filesystem_aliases_and_preserves_internal_dangling_links() {
        let folder = tempfile::tempdir().unwrap();
        fs::create_dir(folder.path().join("a")).unwrap();
        std::os::unix::fs::symlink(".", folder.path().join("a/LINK")).unwrap();
        std::os::unix::fs::symlink("a/link/../..", folder.path().join("escape")).unwrap();
        let links = [("a/LINK".into(), ".".into()), ("escape".into(), "a/link/../..".into())]
            .into_iter()
            .collect();
        // On a case-sensitive volume the different spelling names a missing
        // internal path; on a case-insensitive volume it follows the real link.
        if fs::symlink_metadata(folder.path().join("a/link")).is_ok() {
            assert!(validate_staged_links(folder.path(), &links).is_err());
        }
        std::os::unix::fs::symlink("missing/../internal", folder.path().join("dangling")).unwrap();
        let safe = [("dangling".into(), "missing/../internal".into())]
            .into_iter()
            .collect();
        validate_staged_links(folder.path(), &safe).unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn transient_accept_errors_retry_with_backoff() {
        let started = tokio::time::Instant::now();
        let mut failures = 2;
        let accepted = accept_connection(|| {
            let result = if failures > 0 {
                failures -= 1;
                Err(std::io::Error::from_raw_os_error(libc::EMFILE))
            } else {
                Ok(7)
            };
            std::future::ready(result)
        })
        .await;
        assert_eq!(accepted, 7);
        assert!(started.elapsed() >= std::time::Duration::from_millis(200));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unrelated_addresses_cannot_hold_bulk_slots() {
        use tokio::io::AsyncReadExt;
        let pair = pair_at(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST));
        let mut unrelated = Vec::new();
        for _ in 0..4 {
            unrelated.push(TcpStream::connect(("127.0.0.1", pair.source.port)).await.unwrap());
        }
        for stream in &mut unrelated {
            let result = tokio::time::timeout(std::time::Duration::from_secs(1), stream.read(&mut [0; 1]))
                .await
                .expect("unrelated address held a bulk slot");
            assert!(
                matches!(result, Ok(0))
                    || matches!(result, Err(ref error) if error.kind() == std::io::ErrorKind::ConnectionReset)
            );
        }
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let path = source.path().join("file");
        fs::write(&path, b"connected member").unwrap();
        pair.source.offer(vec![path]).unwrap();
        let remote = remote_offer(&pair.source);
        let hub = pair.destination.clone();
        let output = destination.path().join("file");
        tokio::task::spawn_blocking(move || hub.fetch(&remote, 0, &output, &Progress::default(), |_| false))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fs::read(destination.path().join("file")).unwrap(), b"connected member");
    }
}
