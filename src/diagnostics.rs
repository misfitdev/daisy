//! Opt-in developer traces collected over authenticated sharing links.
//! The event tap uses a separate bounded queue; formatting and I/O happen here.

use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc};
use tracing::{Subscriber, field::Visit};
use tracing_subscriber::layer::{Context as LayerContext, Layer};

use crate::identity::PublicKey;

pub const CAPACITY: usize = 256;
pub const MAX_RECORD: usize = 4096;
const SOCKET: &str = "developer-trace.sock";
static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub sequence: u64,
    pub unix_ms: u64,
    pub elapsed_ms: u64,
    pub build: String,
    pub level: String,
    pub target: String,
    pub fields: BTreeMap<String, String>,
    pub dropped_before: u64,
}

impl Record {
    pub fn bounded(&self) -> bool {
        serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= MAX_RECORD)
    }
}

#[derive(Serialize, Deserialize)]
struct Collected {
    system: String,
    record: Record,
    collector_dropped_before: u64,
}

pub struct Hub {
    started: Instant,
    remote: AtomicUsize,
    sequence: AtomicU64,
    dropped: AtomicU64,
    outgoing_dropped: AtomicU64,
    collector_dropped: AtomicU64,
    collector: Mutex<Option<mpsc::Sender<Collected>>>,
    outgoing: mpsc::Sender<Record>,
    pending: Mutex<mpsc::Receiver<Record>>,
    pub changed: Notify,
}

pub fn hub() -> &'static Arc<Hub> {
    HUB.get_or_init(Hub::new)
}

impl Hub {
    fn new() -> Arc<Self> {
        let (outgoing, pending) = mpsc::channel(CAPACITY);
        Arc::new(Self {
            started: Instant::now(),
            remote: AtomicUsize::new(0),
            sequence: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            outgoing_dropped: AtomicU64::new(0),
            collector_dropped: AtomicU64::new(0),
            collector: Mutex::new(None),
            outgoing,
            pending: Mutex::new(pending),
            changed: Notify::new(),
        })
    }

    pub fn collecting(&self) -> bool {
        self.collector.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    pub fn enabled(&self) -> bool {
        self.remote.load(Ordering::Acquire) != 0 || self.collecting()
    }

    pub fn lease(self: &Arc<Self>) -> Lease {
        Lease {
            hub: self.clone(),
            enabled: false,
        }
    }

    pub fn drain(&self) -> Vec<Record> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        (0..64).filter_map(|_| pending.try_recv().ok()).collect()
    }

    fn publish(&self, mut record: Record) {
        record.dropped_before = record
            .dropped_before
            .saturating_add(self.dropped.swap(0, Ordering::AcqRel));
        if self.remote.load(Ordering::Acquire) != 0 {
            let mut outgoing = record.clone();
            outgoing.dropped_before = outgoing
                .dropped_before
                .saturating_add(self.outgoing_dropped.swap(0, Ordering::AcqRel));
            if let Err(error) = self.outgoing.try_send(outgoing) {
                self.outgoing_dropped
                    .fetch_add(error.into_inner().dropped_before + 1, Ordering::Relaxed);
            }
            self.changed.notify_one();
        }
        // The server supplies the authenticated local key, not the visitor.
        self.collect("local".to_owned(), record);
    }

    pub fn collect(&self, system: String, record: Record) {
        let collector = self.collector.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(sender) = collector.as_ref() {
            let lost = self.collector_dropped.swap(0, Ordering::AcqRel);
            if sender
                .try_send(Collected {
                    system,
                    record,
                    collector_dropped_before: lost,
                })
                .is_err()
            {
                self.collector_dropped.fetch_add(lost + 1, Ordering::Relaxed);
            }
        }
    }
}

pub struct Lease {
    hub: Arc<Hub>,
    enabled: bool,
}

impl Lease {
    pub fn enabled(&self) -> bool {
        self.enabled
    }
    pub fn set(&mut self, enabled: bool) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            self.hub.remote.fetch_add(1, Ordering::AcqRel);
        } else {
            self.hub.remote.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.set(false);
    }
}

pub struct TraceLayer(pub Arc<Hub>);

struct Fields {
    values: BTreeMap<String, String>,
    remaining: usize,
}

impl Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        // A bounded formatter avoids allocating a giant Debug value first.
        let mut text = Limited(String::new(), self.remaining.min(512));
        let _ = fmt::write(&mut text, format_args!("{value:?}"));
        self.remaining = self.remaining.saturating_sub(text.0.len() + field.name().len());
        if self.values.len() < 24 && self.remaining > 0 {
            self.values.insert(field.name().to_owned(), text.0);
        }
    }
}

struct Limited(String, usize);
impl fmt::Write for Limited {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut length = text.len().min(self.1.saturating_sub(self.0.len()));
        while !text.is_char_boundary(length) {
            length -= 1;
        }
        self.0.push_str(&text[..length]);
        if length < text.len() { Err(fmt::Error) } else { Ok(()) }
    }
}

impl<S: Subscriber> Layer<S> for TraceLayer {
    fn on_event(&self, event: &tracing::Event<'_>, _context: LayerContext<'_, S>) {
        let hub = &self.0;
        if !hub.enabled() {
            return;
        }
        let mut fields = Fields {
            values: BTreeMap::new(),
            remaining: 1024,
        };
        event.record(&mut fields);
        let metadata = event.metadata();
        let record = Record {
            sequence: hub.sequence.fetch_add(1, Ordering::Relaxed),
            unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64,
            elapsed_ms: hub.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            build: crate::VERSION.to_owned(),
            level: metadata.level().to_string(),
            target: metadata.target().chars().take(128).collect(),
            fields: fields.values,
            dropped_before: 0,
        };
        if record.bounded() {
            hub.publish(record);
        } else {
            hub.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Enable tracing without changing the normal stderr filter.
pub fn trace_filter(metadata: &tracing::Metadata<'_>) -> bool {
    (metadata.target() == "daisy" || metadata.target().starts_with("daisy::")) && hub().enabled()
}

struct SocketGuard {
    path: PathBuf,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode && m.file_type().is_socket()) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct Server {
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Local activation is restricted to the UID that owns the private socket.
pub async fn serve(home: &Path, key: PublicKey) -> Result<Server> {
    serve_with_hub(home, key, hub().clone()).await
}

async fn serve_with_hub(home: &Path, key: PublicKey, trace: Arc<Hub>) -> Result<Server> {
    let path = home.join(SOCKET);
    if let Ok(metadata) = fs::symlink_metadata(&path) {
        ensure!(metadata.file_type().is_socket(), "trace socket path is not a socket");
        match UnixStream::connect(&path).await {
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => fs::remove_file(&path)?,
            _ => bail!("a developer trace server is already running or inaccessible"),
        }
    }
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let metadata = fs::symlink_metadata(&path)?;
    let uid = metadata.uid();
    let guard = SocketGuard {
        path,
        inode: metadata.ino(),
    };
    let slots = Arc::new(tokio::sync::Semaphore::new(4));
    let task = tokio::spawn(async move {
        let _guard = guard;
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let accepted = tokio::select! {
                accepted = listener.accept() => accepted,
                _ = clients.join_next(), if !clients.is_empty() => continue,
            };
            let Ok((mut stream, _)) = accepted else {
                break;
            };
            if !stream.peer_cred().is_ok_and(|credential| credential.uid() == uid) {
                continue;
            }
            let Ok(slot) = slots.clone().try_acquire_owned() else {
                continue;
            };
            let hub = trace.clone();
            clients.spawn(async move {
                let _slot = slot;
                let mut request = [0];
                if !matches!(tokio::time::timeout(Duration::from_secs(2), stream.read_exact(&mut request)).await, Ok(Ok(_))) || request != [1] { return; }
                let (sender, mut records) = mpsc::channel(CAPACITY);
                {
                    let mut collector = hub.collector.lock().unwrap_or_else(|e| e.into_inner());
                    if collector.is_some() { return; }
                    *collector = Some(sender);
                }
                // Cancellation and all I/O exits disable this collector.
                struct Attached(Arc<Hub>);
                impl Drop for Attached {
                    fn drop(&mut self) {
                        self.0.collector.lock().unwrap_or_else(|e| e.into_inner()).take();
                        self.0.changed.notify_one();
                    }
                }
                let _attached = Attached(hub.clone());
                hub.changed.notify_one();
                hub.publish(Record {
                    sequence: hub.sequence.fetch_add(1, Ordering::Relaxed),
                    unix_ms: SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                    elapsed_ms: hub.started.elapsed().as_millis() as u64,
                    build: crate::VERSION.to_owned(), level: "INFO".into(), target: "daisy::diagnostics".into(),
                    fields: BTreeMap::from([("message".into(), "developer trace collection started".into()),
                        ("protocol".into(), crate::session::PROTOCOL.to_string())]), dropped_before: 0,
                });
                let (mut reader, mut writer) = stream.into_split();
                loop {
                    tokio::select! {
                        _ = reader.read(&mut request) => break,
                        record = records.recv() => {
                            let Some(mut record) = record else { break; };
                            if record.system == "local" { record.system = key.to_hex(); }
                            let Ok(mut bytes) = serde_json::to_vec(&record) else { continue; };
                            bytes.push(b'\n');
                            if !matches!(tokio::time::timeout(Duration::from_secs(2), writer.write_all(&bytes)).await, Ok(Ok(()))) { break; }
                        }
                    }
                }
            });
        }
    });
    Ok(Server { task })
}

/// Attach to a running instance. Ctrl+C/disconnect disables the mesh request.
pub async fn collect(home: &Path, output: &Path) -> Result<()> {
    let mut stream = UnixStream::connect(home.join(SOCKET))
        .await
        .context("start a trace-capable Daisy instance on this system first")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)
        .context("create a new private trace-output file")?;
    stream.write_all(&[1]).await?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    eprintln!("Developer trace enabled across supported connected peers. Press Control+C to stop.");
    loop {
        line.clear();
        let mut bounded = (&mut reader).take((MAX_RECORD + 257) as u64);
        tokio::select! {
            result = bounded.read_until(b'\n', &mut line) => {
                let count = result?;
                ensure!(count != 0, "trace server disconnected; another collector may already be active");
                ensure!(line.len() <= MAX_RECORD + 256, "oversized trace record");
                file.write_all(&line)?;
            }
            result = tokio::signal::ctrl_c() => { result?; file.sync_all()?; return Ok(()); }
        }
    }
}

/// Copy-only event-tap facts. Never stores keycodes, text or clipboard data.
#[derive(Debug, Clone, Copy)]
pub struct CaptureEvent {
    pub at: Duration,
    pub reason: &'static str,
    pub generation: Option<u64>,
    pub event_type: Option<u32>,
    pub source_pid: Option<i64>,
    pub source_state: Option<i64>,
}

pub struct CaptureQueue {
    sender: mpsc::Sender<CaptureEvent>,
    receiver: Mutex<mpsc::Receiver<CaptureEvent>>,
    lost: AtomicU64,
}

impl Default for CaptureQueue {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel(64);
        Self {
            sender,
            receiver: Mutex::new(receiver),
            lost: AtomicU64::new(0),
        }
    }
}

impl CaptureQueue {
    /// No I/O, waiting, formatting, or change to control wakeups.
    pub fn record(&self, event: CaptureEvent) {
        if self.sender.try_send(event).is_err() {
            self.lost.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn drain(&self) {
        let mut receiver = self.receiver.lock().unwrap_or_else(|e| e.into_inner());
        while let Ok(event) = receiver.try_recv() {
            tracing::debug!(at_ms = event.at.as_millis() as u64, reason = event.reason, generation = ?event.generation,
                event_type = ?event.event_type, source_pid = ?event.source_pid, source_state = ?event.source_state,
                "local capture decision");
        }
        let lost = self.lost.swap(0, Ordering::AcqRel);
        if lost > 0 {
            tracing::warn!(lost, "capture diagnostics dropped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::{Layer, layer::SubscriberExt};

    fn record(sequence: u64) -> Record {
        Record {
            sequence,
            unix_ms: 0,
            elapsed_ms: 0,
            build: "test".into(),
            level: "TRACE".into(),
            target: "daisy::test".into(),
            fields: Default::default(),
            dropped_before: 0,
        }
    }

    #[test]
    fn request_leases_enable_trace_only_while_at_least_one_request_is_live() {
        let hub = Hub::new();
        let mut a = hub.lease();
        let mut b = hub.lease();
        assert!(!hub.enabled());
        a.set(true);
        a.set(true);
        b.set(true);
        assert!(hub.enabled());
        a.set(false);
        assert!(hub.enabled());
        drop(b);
        assert!(!hub.enabled());
    }

    #[test]
    fn trace_activation_is_dynamic_and_leaves_the_normal_log_filter_independent() {
        let hub = Hub::new();
        let filter_hub = hub.clone();
        let subscriber = tracing_subscriber::registry().with(TraceLayer(hub.clone()).with_filter(
            tracing_subscriber::filter::dynamic_filter_fn(move |_, _| filter_hub.enabled()),
        ));
        tracing::subscriber::with_default(subscriber, || {
            for enabled in [false, true, false, true] {
                let mut lease = hub.lease();
                lease.set(enabled);
                tracing::trace!(target: "daisy::test", "same trace callsite");
                assert_eq!(hub.drain().len(), usize::from(enabled));
            }
        });
    }

    #[test]
    fn bounded_queues_report_loss_without_blocking_and_debug_values_are_bounded() {
        let hub = Hub::new();
        let mut lease = hub.lease();
        lease.set(true);
        for sequence in 0..CAPACITY + 10 {
            hub.publish(record(sequence as u64));
        }
        assert_eq!(hub.outgoing_dropped.load(Ordering::Relaxed), 10);
        assert_eq!(hub.drain().len(), 64);
        hub.publish(record(1000));
        let mut records = hub.drain();
        while !records.iter().any(|r| r.sequence == 1000) {
            records = hub.drain();
        }
        assert_eq!(records.last().unwrap().dropped_before, 10);
        let subscriber = tracing_subscriber::registry().with(TraceLayer(hub.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "daisy::test", huge = ?"é".repeat(100_000), "large event");
        });
        let record = hub.drain().pop().unwrap();
        assert!(record.bounded());
        assert!(record.fields["huge"].len() <= 512);
    }

    #[test]
    fn oversized_source_event_loss_is_reported_to_a_local_only_collector() {
        struct Raw;
        impl fmt::Debug for Raw {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..480 {
                    fmt::Write::write_str(formatter, "\0")?;
                }
                Ok(())
            }
        }
        let hub = Hub::new();
        let (sender, mut received) = mpsc::channel(CAPACITY);
        *hub.collector.lock().unwrap() = Some(sender);
        let subscriber = tracing_subscriber::registry().with(TraceLayer(hub.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "daisy::test", first = ?Raw, second = ?Raw);
            tracing::trace!(target: "daisy::test", "after oversized record");
        });
        let record = received.try_recv().unwrap();
        assert_eq!(record.record.dropped_before, 1);
        assert!(record.record.fields["message"].contains("after oversized record"));
        assert!(received.try_recv().is_err());
        assert!(hub.drain().is_empty());
    }

    #[test]
    fn capture_diagnostics_are_passive_and_bounded() {
        let key = PublicKey::from_bytes(&[1; 32]).unwrap();
        let control = crate::control::SharedControl::new(key, key);
        let mut wakeups = control.take_wakeups().unwrap();
        for _ in 0..70 {
            control.diagnostics.record(CaptureEvent {
                at: Duration::ZERO,
                reason: "test",
                generation: None,
                event_type: None,
                source_pid: None,
                source_state: None,
            });
        }
        assert_eq!(control.diagnostics.lost.load(Ordering::Relaxed), 6);
        assert!(
            wakeups.try_recv().is_err(),
            "diagnostics must not wake the control-reclaim path"
        );
        assert!(control.physical_activity().is_none());
        assert_eq!(control.state.lock().unwrap().generation(), 0);
    }

    #[tokio::test]
    async fn local_collector_attaches_once_streams_records_and_disables_on_disconnect() {
        let directory = tempfile::tempdir().unwrap();
        let key = PublicKey::from_bytes(&[1; 32]).unwrap();
        let hub = Hub::new();
        let server = serve_with_hub(directory.path(), key, hub.clone()).await.unwrap();
        assert_eq!(
            fs::metadata(directory.path().join(SOCKET))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let mut stream = UnixStream::connect(directory.path().join(SOCKET)).await.unwrap();
        stream.write_all(&[1]).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let header: Collected = serde_json::from_str(&line).unwrap();
        assert_eq!(header.system, key.to_hex());
        assert!(hub.enabled());
        let mut duplicate = UnixStream::connect(directory.path().join(SOCKET)).await.unwrap();
        duplicate.write_all(&[1]).await.unwrap();
        assert_eq!(duplicate.read(&mut [0]).await.unwrap(), 0);
        assert!(
            hub.collecting(),
            "a rejected attachment must not disable the active collector"
        );
        hub.collect("authenticated-peer".into(), record(7));
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let received: Collected = serde_json::from_str(&line).unwrap();
        assert_eq!(received.system, "authenticated-peer");
        assert_eq!(received.record.sequence, 7);
        drop(reader);
        tokio::time::timeout(Duration::from_secs(2), async {
            while hub.collecting() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!hub.enabled());
        drop(server);
        tokio::task::yield_now().await;
        assert!(!directory.path().join(SOCKET).exists());
    }
}
