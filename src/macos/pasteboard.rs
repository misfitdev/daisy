//! The system pasteboard, for clipboard sharing. Carries out
//! `clipboard::Clipboard`; the decisions live in `clipboard`.

use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSPasteboard, NSPasteboardTypePNG,
    NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSDictionary, NSObject, NSObjectNSThreadPerformAdditions, NSObjectProtocol, NSString};

use crate::clipboard::{Clipboard, Content, MAX_IMAGE};

const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";
const MAX_TIFF_INPUT: usize = 64 << 20;

struct Work(std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>);
define_class!(
    // SAFETY: the closure is moved under a mutex and runs once on the main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = AnyThread]
    #[ivars = Work]
    struct MainPasteboardWork;
    unsafe impl NSObjectProtocol for MainPasteboardWork {}
    impl MainPasteboardWork {
        #[unsafe(method(runPasteboardWork))]
        fn run(&self) {
            let work = self.ivars().0.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(work) = work {
                work();
            }
        }
    }
);

/// Keep every Daisy-initiated AppKit pasteboard operation on one thread.
/// Callers must not hold a lock that a main-thread pasteboard callback needs.
pub(super) fn on_main<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    if objc2_foundation::MainThreadMarker::new().is_some() {
        return work();
    }
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let allocated = MainPasteboardWork::alloc().set_ivars(Work(std::sync::Mutex::new(Some(Box::new(move || {
        let _ = sender.send(work());
    })))));
    // SAFETY: NSObject's initializer completes the allocated subclass.
    let request: objc2::rc::Retained<MainPasteboardWork> = unsafe { msg_send![super(allocated), init] };
    // SAFETY: the selector takes no arguments; Foundation retains the target until it runs.
    unsafe {
        request.performSelectorOnMainThread_withObject_waitUntilDone(sel!(runPasteboardWork), None, false);
    }
    receiver.recv().expect("main-thread pasteboard work did not finish")
}

/// A handle without native state, so it can live in the session task.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pasteboard;

/// Screenshots and many apps copy images as TIFF only; Daisy carries PNG.
fn tiff_to_png(tiff: &NSData) -> Option<Vec<u8>> {
    let image = NSBitmapImageRep::imageRepWithData(tiff)?;
    let properties = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
    // SAFETY: an empty properties dictionary is valid for every file type.
    let png = unsafe { image.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties) }?;
    Some(png.to_vec())
}

impl Clipboard for Pasteboard {
    fn change_count(&self) -> i64 {
        on_main(|| NSPasteboard::generalPasteboard().changeCount() as i64)
    }

    fn read(&self) -> Option<Content> {
        finish_read(on_main(read_native))
    }

    fn read_for_peer(&self, files: bool) -> Option<Content> {
        finish_read(on_main(move || {
            if files
                && super::file_pasteboard::available()
                && NSPasteboard::generalPasteboard()
                    .types()
                    .is_some_and(|types| types.iter().any(|t| t.to_string() == "public.file-url"))
            {
                return None;
            }
            read_native()
        }))
    }

    fn write(&mut self, content: &Content) -> i64 {
        let content = content.clone();
        on_main(move || write_native(&content))
    }
}

fn read_native() -> Option<(Content, Option<Vec<u8>>)> {
    let board = NSPasteboard::generalPasteboard();
    // SAFETY: AppKit initialises its pasteboard type constants before any
    // code runs and never changes them.
    let (string, rtf, png, tiff) = unsafe {
        (
            NSPasteboardTypeString,
            NSPasteboardTypeRTF,
            NSPasteboardTypePNG,
            NSPasteboardTypeTIFF,
        )
    };
    let concealed = board.types().is_some_and(|types| {
        types.iter().any(|kind| {
            let kind = kind.to_string();
            kind == CONCEALED_TYPE || kind == TRANSIENT_TYPE
        })
    });
    let text = board.stringForType(string).map(|s| s.to_string());
    let rtf = board.dataForType(rtf).map(|d| d.to_vec());
    let png = board
        .dataForType(png)
        .filter(|d| d.length() <= MAX_IMAGE)
        .map(|d| d.to_vec());
    let tiff = if png.is_none() {
        board
            .dataForType(tiff)
            .filter(|d| d.length() <= MAX_TIFF_INPUT)
            .map(|d| d.to_vec())
    } else {
        None
    };
    let content = Content {
        text,
        rtf,
        png,
        concealed,
    };
    (!content.is_empty() || tiff.is_some()).then_some((content, tiff))
}

fn finish_read(snapshot: Option<(Content, Option<Vec<u8>>)>) -> Option<Content> {
    let (mut content, tiff) = snapshot?;
    if let Some(tiff) = tiff {
        content.png = tiff_to_png(&NSData::with_bytes(&tiff)).filter(|png| png.len() <= MAX_IMAGE);
    }
    (!content.is_empty()).then_some(content)
}

fn write_native(content: &Content) -> i64 {
    let board = NSPasteboard::generalPasteboard();
    // SAFETY: as in `read`.
    let (string, rtf, png) = unsafe { (NSPasteboardTypeString, NSPasteboardTypeRTF, NSPasteboardTypePNG) };
    board.clearContents();
    if let Some(text) = &content.text {
        board.setString_forType(&NSString::from_str(text), string);
    }
    if let Some(bytes) = &content.rtf {
        board.setData_forType(Some(&NSData::with_bytes(bytes)), rtf);
    }
    if let Some(bytes) = &content.png {
        board.setData_forType(Some(&NSData::with_bytes(bytes)), png);
    }
    board.changeCount() as i64
}
