//! Finder-compatible file URLs, supplied on the first native clipboard request.
//! Materialization runs on a worker; a pending provider returns immediately.
//! Completed file URLs are republished only while Daisy still owns the clipboard. Completed staging survives clipboard replacement for this boot.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, ensure};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardItemDataProvider, NSPasteboardTypeFileURL, NSPasteboardWriting,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObject, NSObjectNSThreadPerformAdditions, NSObjectProtocol, NSString, NSURL,
};

use crate::file_cache::Cache;
use crate::file_transfer::{Hub, Progress, Remote};
use crate::files::OfferId;

static NATIVE_FILES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Called before the application's main event loop starts.
pub fn enable(_mtm: MainThreadMarker) {
    NATIVE_FILES.store(true, Ordering::Release);
}

pub fn available() -> bool {
    NATIVE_FILES.load(Ordering::Acquire)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
enum State {
    Pending,
    Running,
    Ready(std::result::Result<PathBuf, String>),
}
struct Job {
    hub: Arc<Hub>,
    remote: Arc<Remote>,
    progress: Arc<Progress>,
    state: Mutex<State>,
    finished_notice: std::sync::atomic::AtomicBool,
    notice_queued: std::sync::atomic::AtomicBool,
    installed: std::sync::atomic::AtomicBool,
}
impl Job {
    fn start(self: &Arc<Self>) {
        let mut state = lock(&self.state);
        if !matches!(*state, State::Pending) {
            return;
        }
        *state = State::Running;
        drop(state);
        let job = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("clipboard-files".into())
            .spawn(move || {
                let result = job.materialize().map_err(|e| format!("{e:#}"));
                if let Err(error) = &result {
                    tracing::warn!(error, "copied-file clipboard request failed");
                }
                *lock(&job.state) = State::Ready(result);
                job.signal();
            })
        {
            *lock(&self.state) = State::Ready(Err(error.to_string()));
            self.signal();
        }
    }
    fn materialize(self: &Arc<Self>) -> Result<PathBuf> {
        let cache = cache()?;
        let bytes = self.remote.offer.items.iter().map(|i| i.bytes).sum();
        let staging = cache.reserve(bytes, self.remote.offer.items.len())?;
        for (index, item) in self.remote.offer.items.iter().enumerate() {
            let folder = staging.path().join(index.to_string());
            std::fs::create_dir(&folder)?;
            self.hub.fetch(
                &self.remote,
                index as u16,
                &folder.join(&item.name),
                &self.progress,
                |_| self.progress.cancelled.load(Ordering::Acquire),
            )?;
        }
        ensure!(
            !self.progress.cancelled.load(Ordering::Acquire),
            "file transfer cancelled"
        );
        cache.publish(staging)
    }
}
struct Ivars {
    job: Arc<Job>,
    item: usize,
}
define_class!(
    // SAFETY: synchronized jobs and immutable indices. A pending callback starts
    // a worker and returns, without network I/O or a nested AppKit event loop.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = Ivars]
    struct Provider;
    unsafe impl NSObjectProtocol for Provider {}
    unsafe impl NSPasteboardItemDataProvider for Provider {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide(&self, _board: Option<&NSPasteboard>, item: &NSPasteboardItem, kind: &NSString) {
            let job = &self.ivars().job;
            if job.progress.cancelled.load(Ordering::Acquire) {
                return;
            }
            job.start();
            let folder = match &*lock(&job.state) {
                State::Ready(Ok(folder)) => Some(folder.clone()),
                _ => None,
            };
            if let Some(folder) = folder {
                let path = folder
                    .join(self.ivars().item.to_string())
                    .join(&job.remote.offer.items[self.ivars().item].name);
                let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
                if let Some(value) = url.absoluteString() {
                    item.setString_forType(&value, kind);
                }
            }
        }
    }
);
struct JobIvars {
    job: Arc<Job>,
}
define_class!(
    // SAFETY: this immutable notification carries only synchronized worker state.
    // The selector is dispatched to main before accessing AppKit controls.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = JobIvars]
    struct CompletionNotice;
    unsafe impl NSObjectProtocol for CompletionNotice {}
    impl CompletionNotice {
        #[unsafe(method(refreshFileTransfer))]
        fn refresh(&self) {
            let Some(_) = MainThreadMarker::new() else { return; };
            let job = &self.ivars().job;
            job.notice_queued.store(false, Ordering::Release);
            let ready = match &*lock(&job.state) {
                State::Ready(result) => Some(result.clone()),
                _ => None,
            };
            let Some(result) = ready else { return; };
            if !job.installed.load(Ordering::Acquire) || job.finished_notice.swap(true, Ordering::AcqRel) {
                return;
            }
            let id = notification_id(job.remote.offer.id);
            match result.and_then(|folder| publish_urls(job, &folder).map_err(|e| format!("{e:#}"))) {
                Ok(true) => {
                    let items = job.remote.offer.items.len();
                    let body = if items == 1 {
                        "One copied item is ready to paste in Finder.".to_owned()
                    } else {
                        format!("{items} copied items are ready to paste in Finder.")
                    };
                    super::file_notification::show(&id, "Copied files ready", &body);
                }
                Ok(false) => {}
                Err(error) if job.progress.cancelled.load(Ordering::Acquire) => {
                    tracing::info!(error, "copied-file transfer cancelled");
                }
                Err(error) => {
                    tracing::warn!(error, "copied-file transfer failed");
                    if owns_clipboard(job) {
                        let recovery = if error.contains("clipboard file cache is full") {
                            "Restart this system to clear cached files, then copy again."
                        } else {
                            "Copy the files again to retry."
                        };
                        super::file_notification::show(&id, "Couldn’t receive copied files", recovery);
                    }
                }
            }
        }
    }
);
impl Job {
    fn signal(self: &Arc<Self>) {
        if self.notice_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let allocated = CompletionNotice::alloc().set_ivars(JobIvars { job: self.clone() });
        // SAFETY: NSObject's initializer completes the immutable subclass.
        let notice: Retained<CompletionNotice> = unsafe { msg_send![super(allocated), init] };
        // SAFETY: the selector takes no argument; Foundation retains its target
        // until this queued main-thread invocation completes.
        unsafe {
            notice.performSelectorOnMainThread_withObject_waitUntilDone(sel!(refreshFileTransfer), None, false);
        }
    }
}
fn publish_urls(job: &Arc<Job>, folder: &std::path::Path) -> Result<bool> {
    ensure!(
        !job.progress.cancelled.load(Ordering::Acquire),
        "file transfer cancelled"
    );
    let mut slot = lock(clipboard());
    let Some(_) = slot
        .as_ref()
        .filter(|current| Arc::ptr_eq(&current.job, job) && current.count == count())
    else {
        return Ok(false);
    };
    let mut urls = Vec::<Retained<ProtocolObject<dyn NSPasteboardWriting>>>::new();
    for (index, item) in job.remote.offer.items.iter().enumerate() {
        let path = folder.join(index.to_string()).join(&item.name);
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        urls.push(ProtocolObject::from_retained(url));
    }
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    ensure!(
        board.writeObjects(&NSArray::from_retained_slice(&urls)),
        "received files could not be put on the clipboard"
    );
    job.hub.received_clipboard(board.changeCount() as i64);
    slot.take();
    Ok(true)
}

fn owns_clipboard(job: &Arc<Job>) -> bool {
    lock(clipboard())
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(&current.job, job) && current.count == count())
}

fn notification_id(id: OfferId) -> String {
    let mut result = String::from("daisy-file-");
    for byte in id {
        use std::fmt::Write;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

pub fn offer_failed(count: i64) {
    super::pasteboard::on_main(move || {
        if NSPasteboard::generalPasteboard().changeCount() as i64 != count {
            return;
        }
        super::file_notification::show(
            &format!("daisy-file-offer-{count}"),
            "Couldn’t share copied files",
            "Copy the files again to retry.",
        );
    });
}
struct Clipboard {
    id: OfferId,
    count: i64,
    _providers: Vec<Retained<Provider>>,
    job: Arc<Job>,
}
// SAFETY: providers hold synchronized jobs and immutable item indices. They
// dispatch UI to main and retain no AppKit view in this cross-thread slot.
unsafe impl Send for Clipboard {}
static CLIPBOARD: OnceLock<Mutex<Option<Clipboard>>> = OnceLock::new();
fn clipboard() -> &'static Mutex<Option<Clipboard>> {
    CLIPBOARD.get_or_init(|| Mutex::new(None))
}
pub fn count() -> i64 {
    super::pasteboard::on_main(|| NSPasteboard::generalPasteboard().changeCount() as i64)
}
pub fn holds(id: OfferId) -> bool {
    super::pasteboard::on_main(move || {
        lock(clipboard())
            .as_ref()
            .is_some_and(|p| p.id == id && p.count == count())
    })
}
pub fn clear(id: OfferId) {
    super::pasteboard::on_main(move || clear_native(id));
}
fn clear_native(id: OfferId) {
    let mut current = lock(clipboard());
    if current.as_ref().is_some_and(|p| p.id == id) {
        if current.as_ref().is_some_and(|p| p.count == count()) {
            NSPasteboard::generalPasteboard().clearContents();
        }
        current.take();
    }
}
pub fn copied_paths_with_count() -> (i64, Result<Vec<PathBuf>>) {
    super::pasteboard::on_main(|| {
        let board = NSPasteboard::generalPasteboard();
        (board.changeCount() as i64, copied_paths_native())
    })
}
fn copied_paths_native() -> Result<Vec<PathBuf>> {
    let board = NSPasteboard::generalPasteboard();
    if board.types().is_some_and(|types| {
        types.iter().any(|t| {
            matches!(
                t.to_string().as_str(),
                "org.nspasteboard.ConcealedType" | "org.nspasteboard.TransientType"
            )
        })
    }) {
        return Ok(Vec::new());
    }
    let Some(items) = board.pasteboardItems() else {
        return Ok(Vec::new());
    };
    ensure!(items.len() <= crate::files::MAX_ITEMS, "too many copied files");
    let mut paths = Vec::new();
    for item in items.iter() {
        // SAFETY: AppKit's immutable exported pasteboard type.
        if let Some(value) = unsafe { item.stringForType(NSPasteboardTypeFileURL) } {
            let url = NSURL::URLWithString(&value).context("copied file has an invalid URL")?;
            ensure!(url.isFileURL(), "copied file URL is not local");
            paths.push(PathBuf::from(
                url.path().context("copied file URL has no path")?.to_string(),
            ));
        }
    }
    Ok(paths)
}
pub fn install(hub: Arc<Hub>, remote: Arc<Remote>) -> Result<()> {
    super::pasteboard::on_main(move || install_native(hub, remote))
}
fn install_native(hub: Arc<Hub>, remote: Arc<Remote>) -> Result<()> {
    let job = Arc::new(Job {
        hub,
        remote: remote.clone(),
        progress: Arc::new(Progress::default()),
        state: Mutex::new(State::Pending),
        finished_notice: std::sync::atomic::AtomicBool::new(false),
        notice_queued: std::sync::atomic::AtomicBool::new(false),
        installed: std::sync::atomic::AtomicBool::new(false),
    });
    let mut providers = Vec::new();
    let mut objects = Vec::<Retained<ProtocolObject<dyn NSPasteboardWriting>>>::new();
    for index in 0..remote.offer.items.len() {
        let allocated = Provider::alloc().set_ivars(Ivars {
            job: job.clone(),
            item: index,
        });
        // SAFETY: NSObject's initializer completes the allocated subclass.
        let provider: Retained<Provider> = unsafe { msg_send![super(allocated), init] };
        let item = NSPasteboardItem::new();
        // SAFETY: this exported type and protocol object have AppKit's lifetime.
        ensure!(
            unsafe {
                item.setDataProvider_forTypes(
                    ProtocolObject::from_ref(&*provider),
                    &NSArray::from_slice(&[NSPasteboardTypeFileURL]),
                )
            },
            "file clipboard provider could not be installed"
        );
        objects.push(ProtocolObject::from_retained(item));
        providers.push(provider);
    }
    let board = NSPasteboard::generalPasteboard();
    board.clearContents();
    ensure!(
        board.writeObjects(&NSArray::from_retained_slice(&objects)),
        "file URLs could not be put on clipboard"
    );
    *lock(clipboard()) = Some(Clipboard {
        id: remote.offer.id,
        count: count(),
        _providers: providers,
        job: job.clone(),
    });
    job.installed.store(true, Ordering::Release);
    if !matches!(*lock(&job.state), State::Pending) {
        job.signal();
    }
    Ok(())
}
fn cache() -> Result<Cache> {
    let root = PathBuf::from(std::env::var_os("HOME").context("this system has no home folder")?)
        .join("Library/Caches/dev.misfit.daisy");
    std::fs::create_dir_all(&root)?;
    let mut boot: libc::timeval = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&boot);
    // SAFETY: KERN_BOOTTIME writes a timeval into this correctly sized buffer.
    let result = unsafe {
        libc::sysctl(
            [libc::CTL_KERN, libc::KERN_BOOTTIME].as_mut_ptr(),
            2,
            (&mut boot as *mut libc::timeval).cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    ensure!(
        result == 0 && length == std::mem::size_of_val(&boot),
        "could not identify this system’s cache session"
    );
    Cache::open(&root.join("clipboard"), &format!("{}-{}", boot.tv_sec, boot.tv_usec))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_ready_success_cannot_publish_clipboard_urls() {
        let home = tempfile::tempdir().unwrap();
        let (_sender, enabled) = tokio::sync::watch::channel(true);
        let (hub, task) = Hub::start(
            crate::identity::Identity::generate().unwrap(),
            crate::device::Signer::generate().unwrap(),
            crate::peers::PeerStore::open(home.path()).unwrap(),
            enabled,
        )
        .unwrap();
        let folder = home.path().join("received");
        let remote = Arc::new(Remote::fixture(crate::files::Offer {
            id: [1; 16],
            port: 1,
            items: vec![crate::files::Item {
                name: "copied.txt".into(),
                directory: false,
                bytes: 1,
            }],
        }));
        let progress = Arc::new(Progress::default());
        progress.cancelled.store(true, Ordering::Release);
        let job = Arc::new(Job {
            hub,
            remote,
            progress,
            state: Mutex::new(State::Ready(Ok(folder.clone()))),
            finished_notice: std::sync::atomic::AtomicBool::new(false),
            notice_queued: std::sync::atomic::AtomicBool::new(false),
            installed: std::sync::atomic::AtomicBool::new(true),
        });
        // The native publication entry point must reject a queued success before
        // touching AppKit or the global pasteboard.
        assert!(publish_urls(&job, &folder).is_err());
        task.abort();
    }
}
