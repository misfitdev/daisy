//! Replaying forwarded input on the Mac being controlled.

// Apple's constant names, kept so they match the SDK headers
#![allow(non_upper_case_globals)]

use std::time::Instant;

use super::ffi::*;
use super::pointer::Magnifier;
use crate::input::Action;
use crate::shake::ShakeDetector;
use crate::swipe::SwipeDetector;

/// Posts input events as if they came from this Mac's own hardware.
pub struct Injector {
    source: CGEventSourceRef,
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
            Action::Scroll { dx, dy } => {
                // SAFETY: plain values
                let event = unsafe {
                    CGEventCreateScrollWheelEvent2(
                        self.source,
                        kCGScrollEventUnitPixel,
                        2,
                        dy.round() as i32,
                        dx.round() as i32,
                        0,
                    )
                };
                post(event);
            }
            Action::Key {
                code,
                down,
                repeat,
                flags,
            } => {
                // SAFETY: plain values
                let event = unsafe { CGEventCreateKeyboardEvent(self.source, code, down) };
                if !event.is_null() {
                    // SAFETY: event was just created
                    unsafe { CGEventSetFlags(event, flags) };
                    self.set(event, kCGKeyboardEventAutorepeat, i64::from(repeat));
                }
                post(event);
            }
            Action::Modifiers { code, flags } => {
                // SAFETY: plain values
                let event = unsafe { CGEventCreateKeyboardEvent(self.source, code, true) };
                if !event.is_null() {
                    // a modifier press is a flags-changed event carrying the new state
                    // SAFETY: event was just created
                    unsafe {
                        CGEventSetType(event, kCGEventFlagsChanged);
                        CGEventSetFlags(event, flags);
                    }
                }
                post(event);
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

impl crate::share::Inject for Injector {
    fn execute(&mut self, action: &Action) {
        Injector::execute(self, action);
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

fn post(event: CGEventRef) {
    if event.is_null() {
        return;
    }
    // SAFETY: event is live until released here
    unsafe {
        CGEventPost(kCGHIDEventTap, event);
        CFRelease(event.cast_const());
    }
}
