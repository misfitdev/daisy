//! Finder-compatible file URLs, supplied on the first native clipboard request.
//! Materialization runs on a worker; a pending provider returns immediately.
//! Completed file URLs are republished only while Daisy still owns the clipboard. Completed staging survives clipboard replacement for this boot.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result, ensure};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSPanel, NSPasteboard, NSPasteboardItem, NSPasteboardItemDataProvider,
    NSPasteboardTypeFileURL, NSPasteboardWriting, NSProgressIndicator, NSTextField, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObject, NSObjectNSThreadPerformAdditions, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, NSURL,
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
    ui_tick: Mutex<std::time::Instant>,
    finished_ui: std::sync::atomic::AtomicBool,
    ui_queued: std::sync::atomic::AtomicBool,
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
        self.signal();
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
                |done| {
                    self.progress.done.store(done, Ordering::Relaxed);
                    let mut tick = lock(&self.ui_tick);
                    if tick.elapsed() >= std::time::Duration::from_millis(100) {
                        *tick = std::time::Instant::now();
                        drop(tick);
                        self.signal();
                    }
                    self.progress.cancelled.load(Ordering::Acquire)
                },
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
    struct ProgressNotice;
    unsafe impl NSObjectProtocol for ProgressNotice {}
    impl ProgressNotice {
        #[unsafe(method(refreshFileTransfer))]
        fn refresh(&self) {
            let Some(mtm) = MainThreadMarker::new() else { return; };
            let job = &self.ivars().job;
            job.ui_queued.store(false, Ordering::Release);
            let ready = match &*lock(&job.state) { State::Ready(result) => Some(result.clone()), _ => None };
            PANELS.with(|panels| {
                let mut panels = panels.borrow_mut();
                if let Some(result) = ready {
                    if !job.installed.load(Ordering::Acquire) { return; }
                    if job.finished_ui.swap(true, Ordering::AcqRel) { return; }
                    let result = result.and_then(|folder| publish_urls(job, &folder).map_err(|e| format!("{e:#}")));
                    match result {
                        Ok(()) => { if let Some(panel) = panels.remove(&job.remote.offer.id) { panel.window.orderOut(None); } },
                        Err(_) if job.progress.cancelled.load(Ordering::Acquire) => {
                            if let Some(panel) = panels.remove(&job.remote.offer.id) { panel.window.orderOut(None); }
                        },
                        Err(error) => {
                            let panel = panels.entry(job.remote.offer.id).or_insert_with(|| Panel::new(mtm,job.clone()));
                            panel.failure(&error);
                        },
                    }
                } else if !job.progress.cancelled.load(Ordering::Acquire) {
                    panels.entry(job.remote.offer.id).or_insert_with(|| Panel::new(mtm,job.clone())).update();
                }
            });
        }
    }
);
impl Job {
    fn signal(self: &Arc<Self>) {
        if self.ui_queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let allocated = ProgressNotice::alloc().set_ivars(JobIvars { job: self.clone() });
        // SAFETY: NSObject's initializer completes the immutable subclass.
        let notice: Retained<ProgressNotice> = unsafe { msg_send![super(allocated), init] };
        // SAFETY: the selector takes no argument; Foundation retains its target
        // until this queued main-thread invocation completes.
        unsafe {
            notice.performSelectorOnMainThread_withObject_waitUntilDone(sel!(refreshFileTransfer), None, false);
        }
    }
}
fn publish_urls(job: &Arc<Job>, folder: &std::path::Path) -> Result<()> {
    ensure!(
        !job.progress.cancelled.load(Ordering::Acquire),
        "file transfer cancelled"
    );
    let mut current = lock(clipboard());
    let Some(current) = current
        .as_mut()
        .filter(|current| Arc::ptr_eq(&current.job, job) && current.count == count())
    else {
        return Ok(());
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
    current.count = count();
    Ok(())
}
struct PanelIvars {
    progress: Arc<Progress>,
    id: OfferId,
    window: Retained<NSPanel>,
}
define_class!(
    // SAFETY: AppKit UI and its actions are confined to the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = PanelIvars]
    struct PanelActions;
    unsafe impl NSObjectProtocol for PanelActions {}
    impl PanelActions {
        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&NSObject>) {
            let _keep_alive = self.retain();
            self.ivars().progress.cancelled.store(true, Ordering::Release);
            self.ivars().window.orderOut(None);
            clear(self.ivars().id);
            PANELS.with(|panels| { panels.borrow_mut().remove(&self.ivars().id); });
        }
    }
);
struct Panel {
    progress: Arc<Progress>,
    total: u64,
    window: Retained<NSPanel>,
    title: Retained<NSTextField>,
    status: Retained<NSTextField>,
    bar: Retained<NSProgressIndicator>,
    button: Retained<NSButton>,
    _actions: Retained<PanelActions>,
}
impl Panel {
    fn new(mtm: MainThreadMarker, job: Arc<Job>) -> Self {
        let total = job.remote.offer.items.iter().map(|i| i.bytes).sum();
        Self::with_progress(mtm, job.progress.clone(), job.remote.offer.id, total)
    }
    fn with_progress(mtm: MainThreadMarker, progress: Arc<Progress>, id: OfferId, total: u64) -> Self {
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect(0., 0., 480., 200.),
            NSWindowStyleMask::Titled | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setTitle(&NSString::from_str("Daisy"));
        window.setFloatingPanel(true);
        window.setBecomesKeyOnlyIfNeeded(true);
        window.setHidesOnDeactivate(false);
        // SAFETY: Rust owns the panel's retain; closing must not release it.
        unsafe {
            window.setReleasedWhenClosed(false);
        }
        let allocated = PanelActions::alloc(mtm).set_ivars(PanelIvars {
            progress: progress.clone(),
            id,
            window: window.clone(),
        });
        // SAFETY: NSObject's initializer completes the allocated subclass.
        let actions: Retained<PanelActions> = unsafe { msg_send![super(allocated), init] };
        let title = NSTextField::labelWithString(&NSString::from_str("Receiving copied files"), mtm);
        title.setFrame(rect(20., 150., 440., 26.));
        let status = NSTextField::wrappingLabelWithString(&NSString::from_str("Preparing files for Paste…"), mtm);
        status.setFrame(rect(20., 78., 440., 60.));
        let bar = NSProgressIndicator::initWithFrame(NSProgressIndicator::alloc(mtm), rect(20., 56., 440., 16.));
        bar.setMinValue(0.);
        bar.setMaxValue(1.);
        bar.setIndeterminate(false);
        // SAFETY: the target is retained by Panel and the action has the required ABI.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Cancel"),
                Some(&*actions),
                Some(sel!(cancel:)),
                mtm,
            )
        };
        button.setFrame(rect(360., 16., 100., 28.));
        if let Some(view) = window.contentView() {
            view.addSubview(&title);
            view.addSubview(&status);
            view.addSubview(&bar);
            view.addSubview(&button);
        }
        window.center();
        window.orderFrontRegardless();
        Self {
            progress,
            total,
            window,
            title,
            status,
            bar,
            button,
            _actions: actions,
        }
    }
    fn update(&self) {
        let bytes = self.total;
        let done = self.progress.done.load(Ordering::Relaxed);
        self.bar.setDoubleValue(done as f64 / bytes.max(1) as f64);
        self.status.setStringValue(&NSString::from_str(&format!(
            "{} of {} received",
            size(done),
            size(bytes)
        )));
    }
    fn failure(&self, error: &str) {
        self.title.setStringValue(&NSString::from_str(
            if self.progress.cancelled.load(Ordering::Acquire) {
                "Transfer cancelled"
            } else {
                "Couldn’t receive copied files"
            },
        ));
        self.status
            .setStringValue(&NSString::from_str(&format!("{error}\nCopy the files again to retry.")));
        self.status.setToolTip(Some(&NSString::from_str(error)));
        self.bar.setHidden(true);
        self.button.setTitle(&NSString::from_str("Close"));
        self.window.orderFrontRegardless();
    }
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
fn size(bytes: u64) -> String {
    if bytes < 1000 {
        format!("{bytes} bytes")
    } else if bytes < 1_000_000 {
        format!("{:.1} KB", bytes as f64 / 1000.)
    } else if bytes < 1_000_000_000 {
        format!("{:.1} MB", bytes as f64 / 1e6)
    } else {
        format!("{:.1} GB", bytes as f64 / 1e9)
    }
}

struct NoticeIvars {
    message: String,
    id: OfferId,
}
define_class!(
    // SAFETY: immutable notice data; presentation is dispatched to main.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = NoticeIvars]
    struct Notice;
    unsafe impl NSObjectProtocol for Notice {}
    impl Notice {
        #[unsafe(method(showFileFailure))]
        fn present(&self) {
            let Some(mtm) = MainThreadMarker::new() else { return; };
            let panel = Panel::with_progress(mtm, Arc::new(Progress::default()), self.ivars().id, 0);
            panel.failure(&self.ivars().message);
            panel.title.setStringValue(&NSString::from_str("Couldn’t share copied files"));
            PANELS.with(|panels| { panels.borrow_mut().insert(self.ivars().id, panel); });
        }
    }
);

pub fn offer_failed(message: String) {
    let mut id = [0; 16];
    if getrandom::fill(&mut id).is_err() {
        return;
    }
    let allocated = Notice::alloc().set_ivars(NoticeIvars { message, id });
    // SAFETY: NSObject's initializer completes this immutable subclass.
    let notice: Retained<Notice> = unsafe { msg_send![super(allocated), init] };
    // SAFETY: the selector takes no argument. Foundation retains its target
    // until the queued main-thread invocation completes.
    unsafe {
        notice.performSelectorOnMainThread_withObject_waitUntilDone(sel!(showFileFailure), None, false);
    }
}
thread_local! { static PANELS: RefCell<BTreeMap<OfferId,Panel>> = const { RefCell::new(BTreeMap::new()) }; }

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
    NSPasteboard::generalPasteboard().changeCount() as i64
}
pub fn holds(id: OfferId) -> bool {
    lock(clipboard())
        .as_ref()
        .is_some_and(|p| p.id == id && p.count == count())
}
pub fn clear(id: OfferId) {
    let mut current = lock(clipboard());
    if current.as_ref().is_some_and(|p| p.id == id) {
        if current.as_ref().is_some_and(|p| p.count == count()) {
            NSPasteboard::generalPasteboard().clearContents();
        }
        current.take();
    }
}
pub fn copied_paths() -> Result<Vec<PathBuf>> {
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
    let job = Arc::new(Job {
        hub,
        remote: remote.clone(),
        progress: Arc::new(Progress::default()),
        state: Mutex::new(State::Pending),
        ui_tick: Mutex::new(std::time::Instant::now()),
        finished_ui: std::sync::atomic::AtomicBool::new(false),
        ui_queued: std::sync::atomic::AtomicBool::new(false),
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
            ui_tick: Mutex::new(std::time::Instant::now()),
            finished_ui: std::sync::atomic::AtomicBool::new(false),
            ui_queued: std::sync::atomic::AtomicBool::new(false),
            installed: std::sync::atomic::AtomicBool::new(true),
        });
        // The native publication entry point must reject a queued success before
        // touching AppKit or the global pasteboard.
        assert!(publish_urls(&job, &folder).is_err());
        task.abort();
    }
}
