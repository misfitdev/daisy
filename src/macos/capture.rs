//! Reading the keyboard and mouse on the driving system.
//!
//! An event tap sees every input event before any app does. While this system
//! has control, events pass through untouched; once the pointer crosses to
//! the peer they are swallowed here and forwarded instead.

// Apple's constant names, kept so they match the SDK headers
#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::thread::JoinHandle;

use anyhow::{Result, anyhow};
use tokio::sync::mpsc;

use super::ffi::*;
use super::swipe;
use crate::control::SharedControl;
use crate::input::{Along, Driver, Rect, Route, ScrollPhase, Side};
use crate::protocol::Message;
use crate::swipe::SwipeStep;

// Every event, filtered in the callback. Private gesture events arrive only
// with this mask; asking for their type bits alone delivered nothing.
const EVENTS_OF_INTEREST: u64 = u64::MAX;

/// A running event tap. Dropping it stops the tap and gives the pointer back
/// to this system.
pub struct Capture {
    driver: Arc<Mutex<Driver>>,
    cursor: Arc<Mutex<Cursor>>,
    run_loop: RunLoop,
    thread: Option<JoinHandle<()>>,
}

/// How far the pinned cursor may drift before it is warped back, in points.
const HOLD_SLACK: f64 = 1.5;

/// This system's cursor while the peer has control: hidden, and pinned
/// where it crossed over. macOS 27 accepts but ignores detaching the cursor
/// from the mouse, so pinning is done by warping it back as it drifts.
#[derive(Default)]
struct Cursor {
    pinned: Option<CGPoint>,
    // CGDisplayHideCursor counts calls, so every hide needs exactly one show
    hidden: bool,
    // how often holding the cursor needed a warp, for diagnosing freezes
    held: u64,
    warped: u64,
}

impl Cursor {
    fn freeze(&mut self, at: CGPoint) {
        // SAFETY: plain values. Hiding re-attaches the cursor to the mouse,
        // so it must come before detaching.
        unsafe {
            if self.hidden {
                0
            } else {
                self.hidden = true;
                CGDisplayHideCursor(CGMainDisplayID())
            };
            CGAssociateMouseAndMouseCursorPosition(false);
        }
        self.pinned = Some(at);
        self.held = 0;
        self.warped = 0;
    }

    fn thaw(&mut self, move_to: Option<CGPoint>) {
        // SAFETY: plain values
        unsafe {
            if let Some(point) = move_to {
                CGWarpMouseCursorPosition(point);
            }
            CGAssociateMouseAndMouseCursorPosition(true);
            if self.hidden {
                CGDisplayShowCursor(CGMainDisplayID());
                self.hidden = false;
            }
        }
        self.pinned = None;
    }

    // If freezing did not take, put the cursor back each time it moves.
    fn hold(&mut self, location: CGPoint) {
        let Some(pinned) = self.pinned else { return };
        self.held += 1;
        // warps land on the pixel grid, so allow a little slack or every event warps
        if (location.x - pinned.x).abs() > HOLD_SLACK || (location.y - pinned.y).abs() > HOLD_SLACK {
            // SAFETY: plain value
            unsafe { CGWarpMouseCursorPosition(pinned) };
            self.warped += 1;
        }
    }
}

// Owned by the tap thread; only ever handed to CFRunLoopStop, which may be
// called from any thread.
struct RunLoop(CFRunLoopRef);
unsafe impl Send for RunLoop {}

struct Context {
    driver: Arc<Mutex<Driver>>,
    cursor: Arc<Mutex<Cursor>>,
    messages: mpsc::Sender<Message>,
    overflow: Arc<AtomicBool>,
    tap: CFMachPortRef,
    control: Arc<SharedControl>,
}

impl Context {
    /// Sends input as part of this system's control, stamped with its
    /// generation. Input made while the peer has control is not sent: it
    /// hands control back to this system instead.
    fn send_stamped(&self, stamp: impl FnOnce(u64) -> Message) -> bool {
        let control = &self.control;
        let Some(state) = try_lock(&control.state) else {
            // The session task holds this lock only briefly: keep this
            // one event here rather than end the session.
            self.release_local();
            return false;
        };
        if !state.owns() {
            drop(state);
            if let Some(mut driver) = try_lock(&self.driver) {
                driver.reclaim();
            }
            if let Some(mut cursor) = try_lock(&self.cursor) {
                cursor.thaw(None);
            }
            control.interrupted.store(true, Ordering::Release);
            control.wake();
            return false;
        }
        let message = stamp(state.generation());
        drop(state);
        self.send(message)
    }

    fn send(&self, message: Message) -> bool {
        if self.messages.try_send(message).is_ok() {
            true
        } else {
            self.recover_local();
            false
        }
    }

    fn stop_for_contention(&self) {
        self.overflow.store(true, Ordering::Release);
    }

    fn cursor_or_recover(&self) -> Option<std::sync::MutexGuard<'_, Cursor>> {
        match try_lock(&self.cursor) {
            Some(cursor) => Some(cursor),
            None => {
                self.recover_local();
                None
            }
        }
    }

    fn recover_local(&self) {
        self.release_local();
        self.overflow.store(true, Ordering::Release);
    }

    fn release_local(&self) {
        if let Some(mut driver) = try_lock(&self.driver) {
            driver.reclaim();
        }
        if let Some(mut cursor) = try_lock(&self.cursor) {
            cursor.thaw(None);
        }
    }
}

impl Capture {
    /// Start tapping input. `side` is where the peer sits; forwarded
    /// input and crossings are sent on `messages`, stamped with the
    /// generation `control` holds.
    pub fn start(
        screen: Rect,
        side: Side,
        messages: mpsc::Sender<Message>,
        control: Arc<SharedControl>,
    ) -> Result<(Self, Arc<AtomicBool>)> {
        allow_background_cursor_changes();
        std::hint::black_box(swipe::can_recognize());
        let driver = Arc::new(Mutex::new(Driver::new(screen, side)));
        let cursor = Arc::new(Mutex::new(Cursor::default()));
        let overflowed = Arc::new(AtomicBool::new(false));
        let (ready, started) = std_mpsc::channel();

        let (thread_driver, thread_cursor) = (driver.clone(), cursor.clone());
        let thread_overflow = overflowed.clone();
        let thread = std::thread::Builder::new()
            .name("input tap".into())
            .spawn(move || run_tap(thread_driver, thread_cursor, messages, thread_overflow, ready, control))?;

        let run_loop = started
            .recv()
            .map_err(|_| anyhow!("the input tap thread exited early"))?
            .map_err(|error| anyhow!(error))?;

        Ok((
            Self {
                driver,
                cursor,
                run_loop,
                thread: Some(thread),
            },
            overflowed,
        ))
    }

    /// The peer handed control back at `along`.
    pub fn leave(&self, along: Along) {
        let point = lock(&self.driver).leave(along);
        lock(&self.cursor).thaw(Some(CGPoint { x: point.0, y: point.1 }));
    }
}

/// Lets this background app hide and freeze the cursor, and stops macOS
/// briefly ignoring the mouse after each warp.
fn allow_background_cursor_changes() {
    super::set_cursor_in_background();
    // SAFETY: the source is created and released here; the interval is a plain value
    unsafe {
        let source = CGEventSourceCreate(kCGEventSourceStateHIDSystemState);
        if !source.is_null() {
            CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0);
            CFRelease(source.cast_const());
        }
    }
}

impl crate::share::Pointer for Capture {
    fn leave(&mut self, along: Along) {
        Capture::leave(self, along);
    }
    fn yield_control(&mut self) {
        lock(&self.driver).reclaim();
        lock(&self.cursor).thaw(None);
    }
    fn arrange(&mut self, side: Side) {
        lock(&self.driver).arrange(side);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        lock(&self.driver).reclaim();
        lock(&self.cursor).thaw(None);
        // SAFETY: the run loop belongs to the tap thread, which is still
        // joinable; stopping it from another thread is allowed
        unsafe { CFRunLoopStop(self.run_loop.0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_tap(
    driver: Arc<Mutex<Driver>>,
    cursor: Arc<Mutex<Cursor>>,
    messages: mpsc::Sender<Message>,
    overflow: Arc<AtomicBool>,
    ready: std_mpsc::Sender<std::result::Result<RunLoop, String>>,
    control: Arc<SharedControl>,
) {
    let context = Box::into_raw(Box::new(Context {
        driver,
        cursor,
        messages,
        overflow,
        tap: std::ptr::null_mut(),
        control,
    }));

    // SAFETY: context lives until after the run loop stops below, and the
    // callback is the only other code that touches it
    unsafe {
        let tap = CGEventTapCreate(
            kCGHIDEventTap,
            kCGHeadInsertEventTap,
            kCGEventTapOptionDefault,
            EVENTS_OF_INTEREST,
            on_event,
            context.cast(),
        );
        if tap.is_null() {
            drop(Box::from_raw(context));
            let _ = ready.send(Err(
                "macOS refused the input tap: Daisy needs Accessibility and Input Monitoring; if both are already switched on, quit and reopen Daisy, or reset its permissions"
                    .into(),
            ));
            return;
        }
        (*context).tap = tap;

        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        let run_loop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
        CGEventTapEnable(tap, true);
        let _ = ready.send(Ok(RunLoop(run_loop)));

        CFRunLoopRun();

        CGEventTapEnable(tap, false);
        CFMachPortInvalidate(tap);
        CFRelease(source.cast_const());
        CFRelease(tap.cast_const());
        drop(Box::from_raw(context));
    }
}

/// Whether macOS lets this process read input now. Input Monitoring can
/// report granted before it applies, until Daisy reopens.
pub fn reads_input() -> bool {
    extern "C" fn ignore(_proxy: *mut c_void, _type: u32, event: CGEventRef, _info: *mut c_void) -> CGEventRef {
        event
    }
    // A listen-only tap never holds up input, even for the moment it exists.
    // SAFETY: the tap is never added to a run loop, and is released here
    unsafe {
        let tap = CGEventTapCreate(
            kCGHIDEventTap,
            kCGHeadInsertEventTap,
            kCGEventTapOptionListenOnly,
            1 << kCGEventKeyDown,
            ignore,
            std::ptr::null_mut(),
        );
        if tap.is_null() {
            return false;
        }
        CFMachPortInvalidate(tap);
        CFRelease(tap.cast_const());
    }
    true
}

extern "C" fn on_event(_proxy: *mut c_void, event_type: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef {
    // a panic must not unwind into CoreGraphics; on any failure, let the event through
    let keep = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: user_info is the Context installed with this tap, alive until the tap is gone
        let context = unsafe { &*(user_info as *const Context) };
        handle(context, event_type, event)
    }))
    .unwrap_or(true);

    if keep { event } else { std::ptr::null_mut() }
}

/// Returns whether this system should still receive the event.
fn handle(context: &Context, event_type: u32, event: CGEventRef) -> bool {
    let here = decide(context, event_type, event);
    if here
        && event_type == kCGEventScrollWheel
        // SAFETY: event is valid for the duration of the callback
        && unsafe { CGEventGetIntegerValueField(event, kCGEventSourceUserData) } != DAISY_EVENT_MARKER
        && let Some(phase) = scroll_phase(event)
        && let Some(mut driver) = try_lock(&context.driver)
    {
        driver.scroll_stayed_here(phase);
    }
    here
}

fn scroll_phase(event: CGEventRef) -> Option<ScrollPhase> {
    // SAFETY: event is valid for the duration of the callback
    unsafe {
        ScrollPhase::from_fields(
            CGEventGetIntegerValueField(event, kCGScrollWheelEventScrollPhase),
            CGEventGetIntegerValueField(event, kCGScrollWheelEventMomentumPhase),
        )
    }
}

fn decide(context: &Context, event_type: u32, event: CGEventRef) -> bool {
    if event_type == kCGEventTapDisabledByTimeout || event_type == kCGEventTapDisabledByUserInput {
        // macOS turns off taps it thinks are too slow; turn it back on
        // SAFETY: the tap outlives every callback it delivers
        unsafe { CGEventTapEnable(context.tap, true) };
        return true;
    }

    // Daisy's posted events must never claim control or be forwarded again.
    if unsafe { CGEventGetIntegerValueField(event, kCGEventSourceUserData) } == DAISY_EVENT_MARKER {
        return true;
    }
    let control = &context.control;
    // Only input events count as physical activity; tap notifications and
    // unrelated WindowServer events do not change ownership.
    let input = matches!(
        event_type,
        kCGEventMouseMoved
            | kCGEventLeftMouseDragged
            | kCGEventRightMouseDragged
            | kCGEventOtherMouseDragged
            | kCGEventLeftMouseDown
            | kCGEventLeftMouseUp
            | kCGEventRightMouseDown
            | kCGEventRightMouseUp
            | kCGEventOtherMouseDown
            | kCGEventOtherMouseUp
            | kCGEventKeyDown
            | kCGEventKeyUp
            | kCGEventFlagsChanged
            | kCGEventScrollWheel
    ) || swipe::is_gesture_event(event);
    if !input {
        return true;
    }
    // Momentum scrolling continues a flick after the fingers lift. It never
    // claims control, and stays on the system the flick began on.
    // SAFETY: event is valid for the duration of the callback
    if event_type == kCGEventScrollWheel
        && unsafe { CGEventGetIntegerValueField(event, kCGScrollWheelEventMomentumPhase) } != 0
    {
        if !try_lock(&control.state).is_some_and(|state| state.owns()) {
            return try_lock(&context.driver).is_none_or(|driver| driver.scroll_is_local());
        }
    } else {
        control.note_physical();
        let Some(mut state) = try_lock(&control.state) else {
            // Remote injection may hold the decision lock briefly. The local
            // event still passes immediately; cleanup and the claim run on the
            // session task, never by waiting in the event tap.
            if let Some(mut driver) = try_lock(&context.driver) {
                driver.reclaim();
            }
            if let Some(mut cursor) = try_lock(&context.cursor) {
                cursor.thaw(None);
            }
            control.interrupted.store(true, Ordering::Release);
            control.wake();
            return true;
        };
        let was_owner = state.owns();
        let claim = state.physical(control.now());
        drop(state);
        if !was_owner {
            control.wake();
            if let Some(mut driver) = try_lock(&context.driver) {
                driver.reclaim();
            }
            // the session task holds the cursor while yielding control;
            // it thaws it there, so a busy lock needs nothing here
            if let Some(mut cursor) = try_lock(&context.cursor) {
                cursor.thaw(None);
            }
            if let Some(generation) = claim {
                context.send(Message::ControlClaim { generation });
            }
            // This first event belongs to the system being touched, including if
            // its pointer happens to be resting at the shared screen edge.
            return true;
        }
    }
    if swipe::is_gesture_event(event) {
        return handle_gesture(context, event);
    }

    // SAFETY: event is valid for the duration of the callback
    let location = unsafe { CGEventGetLocation(event) };
    // SAFETY: event is valid for the duration of the callback
    let route = unsafe {
        let integer = |field| CGEventGetIntegerValueField(event, field);
        let double = |field| CGEventGetDoubleValueField(event, field);
        let Some(mut driver) = try_lock(&context.driver) else {
            context.stop_for_contention();
            return true;
        };
        match event_type {
            kCGEventMouseMoved | kCGEventLeftMouseDragged | kCGEventRightMouseDragged | kCGEventOtherMouseDragged => {
                driver.motion(
                    (location.x, location.y),
                    (double(kCGMouseEventDeltaX), double(kCGMouseEventDeltaY)),
                )
            }
            kCGEventLeftMouseDown | kCGEventRightMouseDown | kCGEventOtherMouseDown => driver.button(
                integer(kCGMouseEventButtonNumber) as u8,
                true,
                integer(kCGMouseEventClickState) as u8,
            ),
            kCGEventLeftMouseUp | kCGEventRightMouseUp | kCGEventOtherMouseUp => driver.button(
                integer(kCGMouseEventButtonNumber) as u8,
                false,
                integer(kCGMouseEventClickState) as u8,
            ),
            kCGEventKeyDown | kCGEventKeyUp => driver.key(
                integer(kCGKeyboardEventKeycode) as u16,
                event_type == kCGEventKeyDown,
                integer(kCGKeyboardEventAutorepeat) != 0,
                CGEventGetFlags(event),
            ),
            kCGEventFlagsChanged => driver.modifiers(integer(kCGKeyboardEventKeycode) as u16, CGEventGetFlags(event)),
            kCGEventScrollWheel => driver.scroll(
                double(kCGScrollWheelEventPointDeltaAxis2),
                double(kCGScrollWheelEventPointDeltaAxis1),
                scroll_phase(event),
            ),
            _ => Route::Local,
        }
    };

    match route {
        Route::Local => true,
        Route::Drop => false,
        Route::Enter { along } => {
            // hide and pin the pointer here while it moves on the peer
            let Some(mut cursor) = context.cursor_or_recover() else {
                return true;
            };
            cursor.freeze(location);
            drop(cursor);
            !context.send_stamped(|generation| Message::Enter { generation, along })
        }
        Route::Forward(event) => {
            if matches!(event, crate::input::InputEvent::Motion { .. }) {
                let Some(mut cursor) = context.cursor_or_recover() else {
                    return true;
                };
                cursor.hold(location);
            }
            !context.send_stamped(|generation| Message::Input { generation, event })
        }
        Route::Reclaim => {
            if let Some(mut cursor) = try_lock(&context.cursor) {
                cursor.thaw(None);
            } else {
                context.stop_for_contention();
            }
            context.send_stamped(|generation| Message::Reclaim { generation });
            false
        }
    }
}

// Swipes act on this system while it has control. Otherwise they are recognized,
// forwarded, and swallowed, so this system's Spaces stay put.
fn handle_gesture(context: &Context, event: CGEventRef) -> bool {
    let Some(mut driver) = try_lock(&context.driver) else {
        context.stop_for_contention();
        return true;
    };
    // before macOS 27 swipes cannot be read, so while the peer has
    // control they are only kept from acting here
    let step = swipe::can_recognize()
        .then(|| swipe::dock_event(event))
        .flatten()
        .and_then(|dock| SwipeStep::from_dock(&dock));
    let Some(step) = step else {
        // companion events go wherever the swipe they belong to goes
        return driver.swipe_is_local();
    };
    let route = driver.swipe(step);
    drop(driver);
    match route {
        Route::Forward(event) => !context.send_stamped(|generation| Message::Input { generation, event }),
        Route::Local => true,
        _ => false,
    }
}

fn lock<T>(state: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // a panic while holding the lock leaves the state usable; keep going
    state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn try_lock<T>(state: &Mutex<T>) -> Option<std::sync::MutexGuard<'_, T>> {
    match state.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_key(byte: u8) -> crate::identity::PublicKey {
        crate::identity::PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    #[test]
    fn injected_events_do_not_claim_control() {
        let control = Arc::new(SharedControl::new(test_key(1), test_key(2)));
        let (messages, mut input) = mpsc::channel(1);
        let context = Context {
            driver: Arc::new(Mutex::new(Driver::new(
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 500.0,
                    height: 500.0,
                },
                Side::Right,
            ))),
            cursor: Arc::new(Mutex::new(Cursor::default())),
            messages,
            overflow: Arc::new(AtomicBool::new(false)),
            tap: std::ptr::null_mut(),
            control: control.clone(),
        };
        // Create a marked event but do not post it into the user's session.
        unsafe {
            let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), 7, true);
            assert!(!event.is_null());
            CGEventSetIntegerValueField(event, kCGEventSourceUserData, DAISY_EVENT_MARKER);
            assert!(handle(&context, kCGEventKeyDown, event));
            CFRelease(event.cast_const());
        }
        assert!(!lock(&control.state).owns());
        assert!(input.try_recv().is_err());
    }

    #[test]
    fn cursor_contention_reclaims_remote_driver() {
        let driver = Arc::new(Mutex::new(Driver::new(
            Rect {
                x: 0.0,
                y: 0.0,
                width: 500.0,
                height: 500.0,
            },
            Side::Left,
        )));
        assert!(matches!(
            lock(&driver).motion((0.0, 250.0), (-3.0, 0.0)),
            Route::Enter { .. }
        ));

        let cursor = Arc::new(Mutex::new(Cursor::default()));
        let (messages, _receiver) = mpsc::channel(1);
        let overflow = Arc::new(AtomicBool::new(false));
        let context = Context {
            driver: driver.clone(),
            cursor,
            messages,
            overflow: overflow.clone(),
            tap: std::ptr::null_mut(),
            control: Arc::new(SharedControl::new(test_key(1), test_key(2))),
        };

        let _held = lock(&context.cursor);
        assert!(context.cursor_or_recover().is_none());
        assert!(!lock(&driver).is_remote());
        assert!(overflow.load(Ordering::Acquire));
    }
}
