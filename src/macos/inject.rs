//! Replaying forwarded input on the following system.

// Apple's constant names, kept so they match the SDK headers
#![allow(non_upper_case_globals)]

use std::time::Instant;

use super::ffi::*;
use super::pointer::Magnifier;
use crate::input::{Action, KeyboardEvent, ScrollPhase};
use crate::shake::ShakeDetector;
use crate::swipe::SwipeDetector;

/// Posts input events as if they came from this system's own hardware.
pub struct Injector {
    source: CGEventSourceRef,
    activity: super::power::UserActivity,
    idle_reset: crate::control::IdleReset,
    idle_reset_ignored: bool,
    started: Instant,
    // macOS 27 ignores synthetic clicks and drags without an event number
    numbered_clicks: bool,
    click_number: i64,
    shake: ShakeDetector,
    // recognizes replayed swipes where they can only become shortcuts
    swipes: SwipeDetector,
    // None when "Shake mouse pointer to locate" is off
    magnifier: Option<Magnifier>,
}

// The event source is only used from the thread driving the injector, and
// CoreGraphics event sources may be used from any thread.
unsafe impl Send for Injector {}

impl Injector {
    pub fn new() -> Self {
        // SAFETY: plain value; a null source is also acceptable to every call below
        let source = unsafe { CGEventSourceCreate(kCGEventSourceStateHIDSystemState) };
        Self {
            source,
            activity: super::power::UserActivity::default(),
            idle_reset: crate::control::IdleReset::default(),
            idle_reset_ignored: false,
            started: Instant::now(),
            numbered_clicks: super::major_version() >= 27,
            click_number: 0,
            shake: ShakeDetector::default(),
            swipes: SwipeDetector::default(),
            magnifier: Magnifier::new(),
        }
    }

    pub fn execute(&mut self, action: &Action) {
        match *action {
            Action::Move { to, delta, dragging } => {
                let event_type = match dragging {
                    None => kCGEventMouseMoved,
                    Some(0) => kCGEventLeftMouseDragged,
                    Some(1) => kCGEventRightMouseDragged,
                    Some(_) => kCGEventOtherMouseDragged,
                };
                let event = self.mouse_event(event_type, to, dragging.unwrap_or(0));
                // apps that read raw motion, like games, look at the deltas
                self.set(event, kCGMouseEventDeltaX, delta.0.round() as i64);
                self.set(event, kCGMouseEventDeltaY, delta.1.round() as i64);
                if dragging.is_some() && self.numbered_clicks {
                    self.set(event, kCGMouseEventNumber, self.click_number);
                }
                post(event);
                if let Some(magnifier) = &self.magnifier
                    && self.shake.feed(delta.0, delta.1, Instant::now())
                {
                    magnifier.shaken();
                }
            }
            Action::Button {
                button,
                down,
                clicks,
                at,
            } => {
                let event_type = match (button, down) {
                    (0, true) => kCGEventLeftMouseDown,
                    (0, false) => kCGEventLeftMouseUp,
                    (1, true) => kCGEventRightMouseDown,
                    (1, false) => kCGEventRightMouseUp,
                    (_, true) => kCGEventOtherMouseDown,
                    (_, false) => kCGEventOtherMouseUp,
                };
                let event = self.mouse_event(event_type, at, button);
                self.set(event, kCGMouseEventClickState, i64::from(clicks));
                if self.numbered_clicks {
                    if down {
                        self.next_click_number();
                    }
                    self.set(event, kCGMouseEventNumber, self.click_number);
                }
                post(event);
            }
            Action::Scroll { dx, dy, phase } => post(scroll_event(self.source, dx, dy, phase)),
            Action::Key { .. } | Action::Modifiers { .. } => {
                if let Some(key) = action.keyboard() {
                    post(keyboard_event(self.source, key));
                }
            }
            // before macOS 27 synthetic swipes cannot open Mission Control and
            // do not animate, so the equivalent shortcuts stand in for them
            Action::Swipe { step } => {
                if super::swipe::can_synthesize() {
                    super::swipe::replay(step);
                } else if let Some(direction) = self.swipes.feed(&step.to_dock()) {
                    super::shortcut::post(direction);
                }
            }
            // handled by the session, not by posting an event
            Action::Leave { .. } => {}
        }
    }

    fn mouse_event(&self, event_type: u32, at: (f64, f64), button: u8) -> CGEventRef {
        // SAFETY: plain values
        unsafe { CGEventCreateMouseEvent(self.source, event_type, CGPoint { x: at.0, y: at.1 }, u32::from(button)) }
    }

    fn set(&self, event: CGEventRef, field: u32, value: i64) {
        if !event.is_null() {
            // SAFETY: event is a live event this injector created
            unsafe { CGEventSetIntegerValueField(event, field, value) };
        }
    }

    fn next_click_number(&mut self) {
        if self.click_number == 0 {
            // the first number must be just above the system's own count
            // SAFETY: plain values
            let system = unsafe {
                [kCGEventLeftMouseDown, kCGEventRightMouseDown, kCGEventOtherMouseDown]
                    .map(|event_type| CGEventSourceCounterForEventType(kCGEventSourceStateHIDSystemState, event_type))
            };
            self.click_number = system.iter().map(|&count| i64::from(count)).sum();
        }
        self.click_number += 1;
    }
}

impl Injector {
    /// Posts a modifier event that repeats the current modifier state. macOS
    /// counts it as input, restarting the idle timer behind the screen saver
    /// and lock, yet nothing changes for any app.
    fn reset_idle_timer(&self) {
        // SAFETY: the event is live until `post` releases it
        unsafe {
            let event = CGEventCreate(self.source);
            if event.is_null() {
                return;
            }
            CGEventSetType(event, kCGEventFlagsChanged);
            CGEventSetFlags(event, CGEventSourceFlagsState(kCGEventSourceStateHIDSystemState));
            post(event);
        }
    }
}

impl crate::share::Inject for Injector {
    fn execute(&mut self, action: &Action) {
        Injector::execute(self, action);
    }

    fn arrived(&mut self) {
        self.activity.note();
        let now = self.started.elapsed();
        if !self.idle_reset.due(now) {
            return;
        }
        let idle = super::power::input_idle();
        if let Some(idle) = idle
            && self.idle_reset.ignored(now, idle)
            && !std::mem::replace(&mut self.idle_reset_ignored, true)
        {
            tracing::warn!(
                idle_ms = idle.as_millis() as u64,
                "macOS ignores idle timer resets; this system's screen saver may start while the group is in use"
            );
        }
        tracing::debug!(
            idle_ms = idle.map(|idle| idle.as_millis() as u64),
            "idle timer reset posted"
        );
        self.reset_idle_timer();
    }
}

impl Default for Injector {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        if !self.source.is_null() {
            // SAFETY: the source was created by this injector and is released once
            unsafe { CFRelease(self.source.cast_const()) };
        }
    }
}

/// A pixel scroll, which macOS marks continuous, carrying the trackpad phase
/// apps read for inertia and rubber-banding.
fn scroll_event(source: CGEventSourceRef, dx: f64, dy: f64, phase: Option<ScrollPhase>) -> CGEventRef {
    // SAFETY: plain values
    let event = unsafe {
        CGEventCreateScrollWheelEvent2(
            source,
            kCGScrollEventUnitPixel,
            2,
            dy.round() as i32,
            dx.round() as i32,
            0,
        )
    };
    if let Some(phase) = phase
        && !event.is_null()
    {
        let (scroll, momentum) = phase.fields();
        // SAFETY: event was just created
        unsafe {
            CGEventSetIntegerValueField(event, kCGScrollWheelEventScrollPhase, scroll);
            CGEventSetIntegerValueField(event, kCGScrollWheelEventMomentumPhase, momentum);
        }
    }
    event
}

fn keyboard_event(source: CGEventSourceRef, key: KeyboardEvent) -> CGEventRef {
    // SAFETY: plain values; the caller releases the returned event.
    let event = unsafe { CGEventCreateKeyboardEvent(source, key.code, key.down) };
    if !event.is_null() {
        // SAFETY: event was just created and is still retained.
        unsafe {
            if key.modifier {
                CGEventSetType(event, kCGEventFlagsChanged);
            }
            CGEventSetFlags(event, key.flags);
            CGEventSetIntegerValueField(event, kCGKeyboardEventAutorepeat, i64::from(key.repeat));
        }
    }
    event
}

fn post(event: CGEventRef) {
    if event.is_null() {
        return;
    }
    // SAFETY: event is live until released here
    unsafe {
        CGEventSetIntegerValueField(event, kCGEventSourceUserData, DAISY_EVENT_MARKER);
        CGEventPost(kCGHIDEventTap, event);
        CFRelease(event.cast_const());
    }
}

#[cfg(test)]
mod tests {
    use objc2::encode::{Encode, Encoding};
    use objc2::rc::Retained;
    use objc2::{ClassType, msg_send};
    use objc2_app_kit::{NSEvent, NSEventPhase};

    use super::*;

    #[repr(transparent)]
    struct Event(CGEventRef);

    // SAFETY: a CGEventRef is a pointer to the opaque __CGEvent struct
    unsafe impl Encode for Event {
        const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("__CGEvent", &[]));
    }

    #[test]
    fn appkit_reads_every_keyboard_modifier_combination_and_repeat() {
        use objc2_app_kit::NSEventType;
        let source = unsafe { CGEventSourceCreate(kCGEventSourceStateHIDSystemState) };
        let masks = [0x0002_0000, 0x0004_0000, 0x0008_0000, 0x0010_0000, 0x0080_0000];
        let all_flags = masks.iter().fold(0, |flags, mask| flags | mask);
        for subset in 0..32 {
            let flags = masks.iter().enumerate().fold(0, |flags, (i, mask)| {
                flags | if subset & (1 << i) != 0 { *mask } else { 0 }
            });
            for code in 0..128 {
                // Modifier transitions have their own flags-changed matrix below.
                if matches!(code, 54..=63) {
                    continue;
                }
                for (down, repeat) in [(true, false), (true, true), (false, false)] {
                    let key = KeyboardEvent {
                        code,
                        down,
                        repeat,
                        flags,
                        modifier: false,
                    };
                    let event = keyboard_event(source, key);
                    assert!(!event.is_null());
                    let converted: Option<Retained<NSEvent>> =
                        unsafe { msg_send![NSEvent::class(), eventWithCGEvent: Event(event)] };
                    unsafe { CFRelease(event.cast_const()) };
                    let event = converted.expect("AppKit must accept the generated keyboard event");
                    assert_eq!(
                        event.r#type(),
                        if down { NSEventType::KeyDown } else { NSEventType::KeyUp },
                        "code {code}, flags {flags:#x}",
                    );
                    assert_eq!(event.keyCode(), code);
                    let observed = event.modifierFlags().bits() as u64 & all_flags;
                    // AppKit adds the secondary-function flag for navigation and
                    // function key codes, even when the physical Fn key is up.
                    assert_eq!(
                        observed & !0x0080_0000,
                        flags & !0x0080_0000,
                        "code {code}, subset {subset}"
                    );
                    assert_eq!(observed & flags, flags, "requested modifiers must survive");
                    if down {
                        assert_eq!(event.isARepeat(), repeat);
                    }
                }
            }
        }
        if !source.is_null() {
            unsafe { CFRelease(source.cast_const()) };
        }
    }

    #[test]
    fn appkit_reads_left_and_right_modifier_transitions() {
        use objc2_app_kit::NSEventType;
        for (code, flag) in [
            (55, 0x0010_0008),
            (54, 0x0010_0010),
            (56, 0x0002_0002),
            (60, 0x0002_0004),
            (59, 0x0004_0001),
            (62, 0x0004_2000),
            (58, 0x0008_0020),
            (61, 0x0008_0040),
            (63, 0x0080_0000),
            (57, 0x0001_0000),
        ] {
            for flags in [flag, 0] {
                let action = Action::Modifiers { code, flags };
                let event = keyboard_event(std::ptr::null_mut(), action.keyboard().unwrap());
                assert!(!event.is_null());
                assert_eq!(unsafe { CGEventGetFlags(event) }, flags);
                let converted: Option<Retained<NSEvent>> =
                    unsafe { msg_send![NSEvent::class(), eventWithCGEvent: Event(event)] };
                unsafe { CFRelease(event.cast_const()) };
                let event = converted.expect("AppKit must accept the generated modifier event");
                assert_eq!(event.r#type(), NSEventType::FlagsChanged);
                assert_eq!(event.keyCode(), code);
                assert_eq!(event.modifierFlags().bits() as u64 & 0x00ff_0000, flags & 0x00ff_0000);
            }
        }
    }

    fn appkit_event(phase: Option<ScrollPhase>) -> Retained<NSEvent> {
        let event = scroll_event(std::ptr::null_mut(), 3.0, -12.0, phase);
        assert!(!event.is_null());
        // SAFETY: event is live; AppKit retains what it needs
        let converted: Option<Retained<NSEvent>> =
            unsafe { msg_send![NSEvent::class(), eventWithCGEvent: Event(event)] };
        // SAFETY: event was created above and is released once
        unsafe { CFRelease(event.cast_const()) };
        converted.expect("AppKit reads a scroll event")
    }

    #[test]
    fn appkit_reads_a_replayed_scroll_as_trackpad_scrolling() {
        for (phase, gesture, momentum) in [
            (ScrollPhase::MayBegin, NSEventPhase::MayBegin, NSEventPhase::None),
            (ScrollPhase::Began, NSEventPhase::Began, NSEventPhase::None),
            (ScrollPhase::Changed, NSEventPhase::Changed, NSEventPhase::None),
            (ScrollPhase::Ended, NSEventPhase::Ended, NSEventPhase::None),
            (ScrollPhase::Cancelled, NSEventPhase::Cancelled, NSEventPhase::None),
            (ScrollPhase::MomentumBegan, NSEventPhase::None, NSEventPhase::Began),
            (ScrollPhase::Momentum, NSEventPhase::None, NSEventPhase::Changed),
            (ScrollPhase::MomentumEnded, NSEventPhase::None, NSEventPhase::Ended),
        ] {
            let event = appkit_event(Some(phase));
            assert_eq!(event.phase(), gesture, "{phase:?}");
            assert_eq!(event.momentumPhase(), momentum, "{phase:?}");
            assert!(event.hasPreciseScrollingDeltas(), "{phase:?}");
            assert_eq!((event.scrollingDeltaX(), event.scrollingDeltaY()), (3.0, -12.0));
        }

        let wheel = appkit_event(None);
        assert_eq!(wheel.phase(), NSEventPhase::None);
        assert_eq!(wheel.momentumPhase(), NSEventPhase::None);
    }
}
