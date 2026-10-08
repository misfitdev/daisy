//! Form sheets drawn like the Advanced sheet: a title, a message, the form,
//! an inline error and right-aligned buttons, so a mistake is corrected in
//! place instead of reopening the dialog.

use std::cell::RefCell;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSLineBreakMode, NSModalResponse, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSSize, NSString};

use super::coral_color;
use super::window::{Panel, button, label, rect};

const WIDTH: f64 = 380.0;
const MARGIN: f64 = 20.0;

type Confirm = Box<dyn Fn() -> Result<(), String>>;

#[derive(Default)]
pub struct SheetIvars {
    window: RefCell<Option<Retained<NSWindow>>>,
    error: RefCell<Option<Retained<NSTextField>>>,
    confirm: RefCell<Option<Confirm>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; sheets are used only
    // on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SheetIvars]
    /// One form sheet and the target of its buttons.
    pub struct Sheet;

    unsafe impl NSObjectProtocol for Sheet {}

    impl Sheet {
        #[unsafe(method(confirm:))]
        fn confirm(&self, _sender: Option<&AnyObject>) {
            let result = self.ivars().confirm.borrow().as_ref().map_or(Ok(()), |confirm| confirm());
            match result {
                Ok(()) => self.close(),
                Err(message) => self.show_error(&message),
            }
        }

        #[unsafe(method(cancel:))]
        fn cancel(&self, _sender: Option<&AnyObject>) {
            self.close();
        }

        // Radio buttons need a shared action to act as one group.
        #[unsafe(method(choose:))]
        fn choose(&self, _sender: Option<&AnyObject>) {}
    }
);

impl Sheet {
    /// A sheet whose form controls can target it before it is presented.
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SheetIvars::default());
        // SAFETY: NSObject's init on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    /// Shows the sheet over `parent` with `form` under the message. `confirm`
    /// runs when the primary button is pressed: `Ok` closes the sheet, and
    /// `Err` shows its message under the form and keeps the sheet open.
    #[allow(clippy::too_many_arguments)]
    pub fn present(
        &self,
        parent: &NSWindow,
        title: &str,
        message: &str,
        form: &NSView,
        focus: Option<&NSView>,
        confirm_title: &str,
        confirm: impl Fn() -> Result<(), String> + 'static,
    ) {
        let mtm = self.mtm();
        let content = WIDTH - 2.0 * MARGIN;
        // SAFETY: plain values; the sheet keeps its window until it closes.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, WIDTH, 200.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: the sheet holds the window, so AppKit must not release it on close.
        unsafe { window.setReleasedWhenClosed(false) };
        let root = Panel::new(mtm, rect(0.0, 0.0, WIDTH, 200.0), None, 0.0);
        window.setContentView(Some(&root));

        let mut y = MARGIN;
        let heading = label(title, 15.0, true, mtm);
        heading.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        heading.setFrame(rect(MARGIN, y, content, 22.0));
        root.addSubview(&heading);
        y += 22.0 + 4.0;
        let detail = label(message, 13.0, false, mtm);
        detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        detail.setFrame(rect(MARGIN, y, content, 18.0));
        root.addSubview(&detail);
        y += 18.0 + 14.0;

        let size = form.frame().size;
        form.setFrameOrigin(NSPoint::new(MARGIN, y));
        root.addSubview(form);
        y += size.height + 6.0;
        let error = label("", 11.0, false, mtm);
        error.setTextColor(Some(&NSColor::systemRedColor()));
        error.setFrame(rect(MARGIN, y, content, 16.0));
        error.setHidden(true);
        root.addSubview(&error);
        y += 16.0 + 14.0;

        let target: &AnyObject = self;
        let primary = button(confirm_title, sel!(confirm:), target, mtm);
        primary.setKeyEquivalent(&NSString::from_str("\r"));
        primary.setBezelColor(Some(&coral_color()));
        primary.setContentTintColor(Some(&NSColor::whiteColor()));
        primary.setFrame(rect(WIDTH - MARGIN - 96.0, y, 96.0, 32.0));
        root.addSubview(&primary);
        let cancel = button("Cancel", sel!(cancel:), target, mtm);
        cancel.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        cancel.setFrame(rect(WIDTH - MARGIN - 96.0 - 8.0 - 96.0, y, 96.0, 32.0));
        root.addSubview(&cancel);
        y += 32.0 + MARGIN;

        window.setContentSize(NSSize::new(WIDTH, y));
        root.setFrameSize(NSSize::new(WIDTH, y));
        if let Some(focus) = focus {
            window.setInitialFirstResponder(Some(focus));
        }
        *self.ivars().window.borrow_mut() = Some(window.clone());
        *self.ivars().error.borrow_mut() = Some(error);
        *self.ivars().confirm.borrow_mut() = Some(Box::new(confirm));
        // The buttons do not retain their target, so the completion handler
        // keeps the sheet alive until it closes.
        let keep = self.retain();
        let handler = RcBlock::new(move |_response: NSModalResponse| {
            keep.ivars().confirm.borrow_mut().take();
        });
        parent.beginSheet_completionHandler(&window, Some(&handler));
    }

    fn show_error(&self, message: &str) {
        if let Some(error) = self.ivars().error.borrow().as_ref() {
            error.setStringValue(&NSString::from_str(message));
            error.setHidden(false);
        }
    }

    fn close(&self) {
        let Some(window) = self.ivars().window.borrow_mut().take() else {
            return;
        };
        if let Some(parent) = window.sheetParent() {
            parent.endSheet(&window);
        }
    }
}
