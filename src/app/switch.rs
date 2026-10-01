//! An on/off switch in Daisy's coral, drawn by Daisy: AppKit's switch takes
//! its color from the app's accent, which needs a compiled asset catalog.
//! Sized and shaped like the small switches in System Settings on macOS 27.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityCheckBoxRole, NSAccessibilityRole, NSAccessibilitySubrole,
    NSAccessibilitySwitchSubrole, NSApplication, NSBezierPath, NSColor, NSEvent, NSGraphicsContext, NSResponder,
    NSShadow, NSView, NSWorkspace,
};
use objc2_foundation::{MainThreadMarker, NSCopying, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSTimer};

use super::coral_color;

const TRACK: NSSize = NSSize::new(36.0, 16.0);
const KNOB_INSET: f64 = 1.5;
/// The knob is a capsule this share of the track's width.
const KNOB_WIDTH: f64 = 0.6;
/// Apple publishes no switch timing; AppKit animates for 0.25 s by default,
/// and this sits just past it with an ease in and out.
const SLIDE: Duration = Duration::from_millis(300);

pub struct SwitchIvars {
    on: Cell<bool>,
    /// Where the knob is drawn, 0 off to 1 on, and the slide under way.
    position: Cell<f64>,
    slide: Cell<Option<(Instant, f64)>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    target: RefCell<Weak<AnyObject>>,
    action: Cell<Option<Sel>>,
}

define_class!(
    // SAFETY: NSView has no additional subclassing requirements; the switch
    // is used only on the main thread.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SwitchIvars]
    pub struct Switch;

    unsafe impl NSObjectProtocol for Switch {}

    impl Switch {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let track = self.track();
            let position = self.ivars().position.get();
            let off = NSColor::systemGrayColor().colorWithAlphaComponent(0.28);
            let fill = off.blendedColorWithFraction_ofColor(position, &coral_color()).unwrap_or(off);
            fill.setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(track, TRACK.height / 2.0, TRACK.height / 2.0).fill();

            let size = NSSize::new(TRACK.width * KNOB_WIDTH, TRACK.height - 2.0 * KNOB_INSET);
            let travel = TRACK.width - 2.0 * KNOB_INSET - size.width;
            let x = track.origin.x + KNOB_INSET + travel * position;
            let knob = NSRect::new(NSPoint::new(x, track.origin.y + KNOB_INSET), size);
            NSGraphicsContext::saveGraphicsState_class();
            let shadow = NSShadow::new();
            shadow.setShadowBlurRadius(1.5);
            shadow.setShadowOffset(NSSize::new(0.0, -0.5));
            shadow.setShadowColor(Some(&NSColor::blackColor().colorWithAlphaComponent(0.3)));
            shadow.set();
            NSColor::whiteColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(knob, size.height / 2.0, size.height / 2.0).fill();
            NSGraphicsContext::restoreGraphicsState_class();
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            self.toggle();
        }

        // Like AppKit's controls, only reachable by Tab, and only ringed,
        // when Keyboard navigation is on in System Settings.
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            NSApplication::sharedApplication(self.mtm()).isFullKeyboardAccessEnabled()
        }

        #[unsafe(method(slideStep:))]
        fn slide_step(&self, _timer: &NSTimer) {
            let Some((started, from)) = self.ivars().slide.get() else {
                return;
            };
            let to = if self.ivars().on.get() { 1.0 } else { 0.0 };
            let progress = (started.elapsed().as_secs_f64() / SLIDE.as_secs_f64()).min(1.0);
            let eased = if progress < 0.5 {
                4.0 * progress.powi(3)
            } else {
                1.0 - (-2.0 * progress + 2.0).powi(3) / 2.0
            };
            self.ivars().position.set(from + (to - from) * eased);
            self.setNeedsDisplay(true);
            if progress >= 1.0 {
                self.ivars().slide.set(None);
                if let Some(timer) = self.ivars().timer.take() {
                    timer.invalidate();
                }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            if event.charactersIgnoringModifiers().is_some_and(|keys| keys.to_string() == " ") {
                self.toggle();
            } else {
                // SAFETY: NSView implements keyDown:
                let _: () = unsafe { msg_send![super(self), keyDown: event] };
            }
        }

        #[unsafe(method(focusRingMaskBounds))]
        fn focus_ring_mask_bounds(&self) -> NSRect {
            self.track()
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.track(), TRACK.height / 2.0, TRACK.height / 2.0)
                .fill();
        }

        #[unsafe(method(isAccessibilityElement))]
        fn is_accessibility_element(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityRole))]
        fn accessibility_role(&self) -> Option<Retained<NSAccessibilityRole>> {
            // SAFETY: an AppKit constant
            Some(unsafe { NSAccessibilityCheckBoxRole }.copy())
        }

        #[unsafe(method_id(accessibilitySubrole))]
        fn accessibility_subrole(&self) -> Option<Retained<NSAccessibilitySubrole>> {
            // SAFETY: an AppKit constant
            Some(unsafe { NSAccessibilitySwitchSubrole }.copy())
        }

        #[unsafe(method_id(accessibilityValue))]
        fn accessibility_value(&self) -> Option<Retained<AnyObject>> {
            Some(Retained::into_super(Retained::into_super(NSNumber::new_bool(self.ivars().on.get()))).into())
        }

        #[unsafe(method(accessibilityPerformPress))]
        fn accessibility_perform_press(&self) -> bool {
            self.toggle();
            true
        }
    }
);

impl Switch {
    /// Sends `action` to `target` whenever the person flips it.
    pub fn new(mtm: MainThreadMarker, on: bool, target: &AnyObject, action: Sel, label: &str) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SwitchIvars {
            on: Cell::new(on),
            position: Cell::new(if on { 1.0 } else { 0.0 }),
            slide: Cell::new(None),
            timer: RefCell::new(None),
            target: RefCell::new(Weak::from(target)),
            action: Cell::new(Some(action)),
        });
        let frame = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(TRACK.width + 4.0, TRACK.height + 4.0),
        );
        // SAFETY: initWithFrame: is NSView's designated initializer
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this.setAccessibilityLabel(Some(&objc2_foundation::NSString::from_str(label)));
        this
    }

    pub fn is_on(&self) -> bool {
        self.ivars().on.get()
    }

    /// Shows `on` at once, as for a setting loaded or changed elsewhere.
    pub fn set_on(&self, on: bool) {
        if self.ivars().on.replace(on) != on && self.ivars().slide.get().is_none() {
            self.ivars().position.set(if on { 1.0 } else { 0.0 });
            self.setNeedsDisplay(true);
        }
    }

    fn toggle(&self) {
        let on = !self.is_on();
        self.ivars().on.set(on);
        let reduce_motion = NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion();
        if reduce_motion {
            self.ivars().position.set(if on { 1.0 } else { 0.0 });
            self.setNeedsDisplay(true);
        } else {
            self.ivars()
                .slide
                .set(Some((Instant::now(), self.ivars().position.get())));
        }
        if !reduce_motion && self.ivars().timer.borrow().is_none() {
            // SAFETY: the switch implements slideStep:; the timer is
            // invalidated when the slide ends
            let timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    1.0 / 120.0,
                    self,
                    objc2::sel!(slideStep:),
                    None,
                    true,
                )
            };
            *self.ivars().timer.borrow_mut() = Some(timer);
        }
        let target = self.ivars().target.borrow().load();
        if let (Some(target), Some(action)) = (target, self.ivars().action.get()) {
            // SAFETY: the target implements the action with an object sender
            unsafe {
                NSApplication::sharedApplication(self.mtm()).sendAction_to_from(action, Some(&target), Some(self));
            }
        }
    }

    fn track(&self) -> NSRect {
        let bounds = self.bounds();
        NSRect::new(
            NSPoint::new(
                bounds.origin.x + bounds.size.width - TRACK.width - 2.0,
                bounds.origin.y + (bounds.size.height - TRACK.height) / 2.0,
            ),
            TRACK,
        )
    }
}
