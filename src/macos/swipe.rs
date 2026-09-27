//! Reading and synthesizing the private events behind trackpad swipes.
//!
//! Undocumented WindowServer details are isolated here and verified on hardware.
//! Re-derive and retest them for each supported macOS release.

// Apple's naming, kept so these can be matched against iss and the headers
#![allow(non_upper_case_globals)]

use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::Instant;

use super::ffi::*;
use crate::swipe::{DockEvent, MOTION_HORIZONTAL, MOTION_VERTICAL, PHASE_ENDED, Pace, Pacer, SwipeAxis, SwipeStep};

const kFieldCGSEventType: u32 = 55;
const kFieldGestureHIDType: u32 = 110;
const kFieldSwipeMotion: u32 = 123;
const kFieldSwipeProgress: u32 = 124;
const kFieldSwipePositionX: u32 = 125;
const kFieldSwipePositionY: u32 = 126;
const kFieldSwipeVelocityX: u32 = 129;
const kFieldSwipeVelocityY: u32 = 130;
const kFieldGesturePhase: u32 = 132;
const kFieldGesturePhaseAlias: u32 = 134;
const kFieldZoomDeltaY: u32 = 138;
const kFieldSourceTimestamp: u32 = 169;
const kFieldRawIOHIDPayload: u32 = 4205;

const kCGSEventGesture: i64 = 29;
const kCGSEventDockControl: i64 = 30;
const kHIDEventTypeDockSwipe: i64 = 23;

/// Whether swipes can be synthesized here. Before macOS 27 they cannot open
/// Mission Control and switch Spaces without animation, so `shortcut` stands
/// in for them.
pub fn can_synthesize() -> bool {
    super::major_version() >= 27
}

/// Whether real swipes can be recognized here. Only macOS 27's event stream
/// has been verified.
pub fn can_recognize() -> bool {
    super::major_version() >= 27
}

/// Whether `event` is a DockControl or companion gesture event.
pub(super) fn is_gesture_event(event: CGEventRef) -> bool {
    // SAFETY: event is live for the caller
    let kind = unsafe { CGEventGetIntegerValueField(event, kFieldCGSEventType) };
    kind == kCGSEventDockControl || kind == kCGSEventGesture
}

/// The swipe fields of a DockControl event, or `None` for anything else.
pub(super) fn dock_event(event: CGEventRef) -> Option<DockEvent> {
    // SAFETY: event is live for the caller
    unsafe {
        if CGEventGetIntegerValueField(event, kFieldCGSEventType) != kCGSEventDockControl
            || CGEventGetIntegerValueField(event, kFieldGestureHIDType) != kHIDEventTypeDockSwipe
        {
            return None;
        }
        Some(DockEvent {
            motion: CGEventGetIntegerValueField(event, kFieldSwipeMotion),
            phase: CGEventGetIntegerValueField(event, kFieldGesturePhase),
            progress: CGEventGetDoubleValueField(event, kFieldSwipeProgress),
            velocity_x: CGEventGetDoubleValueField(event, kFieldSwipeVelocityX),
            velocity_y: CGEventGetDoubleValueField(event, kFieldSwipeVelocityY),
        })
    }
}

/// An owned CGEvent, released on drop.
pub struct Event(CGEventRef);

impl Event {
    pub fn as_ptr(&self) -> CGEventRef {
        self.0
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: owned and released exactly once
        unsafe { CFRelease(self.0.cast_const()) };
    }
}

/// Replay one step of a swipe captured on the other Mac, live, so the Dock
/// follows it as it would the fingers. Only for macOS 27 and later; see
/// `can_synthesize`.
///
/// Returns at once: a posting thread spaces the steps out as the Dock needs,
/// keeping them in order without stalling the caller.
pub fn replay(step: SwipeStep) {
    static POSTER: OnceLock<Option<Sender<SwipeStep>>> = OnceLock::new();
    let poster = POSTER.get_or_init(|| {
        let (sender, receiver) = channel();
        std::thread::Builder::new()
            .name("swipe poster".into())
            .spawn(move || pace(&receiver))
            .ok()
            .map(|_| sender)
    });
    match poster {
        Some(sender) if sender.send(step).is_ok() => {}
        _ => tracing::warn!(?step, "the swipe poster is not running"),
    }
}

fn pace(steps: &Receiver<SwipeStep>) {
    let mut pacer = Pacer::default();
    loop {
        let arrived = match pacer.next(Instant::now()) {
            Pace::Post(step) => {
                post_step(&step);
                continue;
            }
            Pace::Wait(wait) => match steps.recv_timeout(wait) {
                Ok(step) => step,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return,
            },
            Pace::Idle => match steps.recv() {
                Ok(step) => step,
                Err(_) => return,
            },
        };
        pacer.push(arrived);
        // a burst gets coalesced as a whole, not a step at a time
        while let Ok(step) = steps.try_recv() {
            pacer.push(step);
        }
    }
}

fn post_step(step: &SwipeStep) {
    let Some((dock, companion)) = step_events(step) else {
        tracing::warn!(?step, "could not create the events for a swipe step");
        return;
    };
    // SAFETY: both events are live until dropped here
    unsafe {
        CGEventPost(kCGSessionEventTap, dock.as_ptr());
        CGEventPost(kCGSessionEventTap, companion.as_ptr());
    }
}

fn new_event() -> Option<Event> {
    // SAFETY: a null source is allowed
    let event = unsafe { CGEventCreate(std::ptr::null_mut()) };
    (!event.is_null()).then_some(Event(event))
}

fn companion_event() -> Option<Event> {
    let event = new_event()?;
    // SAFETY: event was just created
    unsafe { CGEventSetIntegerValueField(event.as_ptr(), kFieldCGSEventType, kCGSEventGesture) };
    Some(event)
}

/// The DockControl event, with its payload, and the companion event that
/// replay `step`.
///
/// Measured on macOS 27 hardware: synthetic swipes read the sign the
/// opposite way from real ones on both axes. A real swipe left or up reports
/// negative progress; a synthetic one needs positive, which went to the left
/// Space and opened Mission Control. So progress and velocity are negated.
fn step_events(step: &SwipeStep) -> Option<(Event, Event)> {
    let phase = step.to_dock().phase;
    let horizontal = step.axis == SwipeAxis::Horizontal;
    let dock = dock_swipe_event(horizontal, phase, -step.progress, -step.velocity)?;
    Some((dock, companion_event()?))
}

/// A DockSwipe event with `progress` and `velocity` already in the
/// synthetic sign convention, with its IOHID payload.
fn dock_swipe_event(horizontal: bool, phase: i64, progress: f64, velocity: f64) -> Option<Event> {
    let event = new_event()?;
    let motion = if horizontal { MOTION_HORIZONTAL } else { MOTION_VERTICAL };
    // SAFETY: event is live
    unsafe {
        CGEventSetIntegerValueField(event.as_ptr(), kFieldCGSEventType, kCGSEventDockControl);
        CGEventSetIntegerValueField(event.as_ptr(), kFieldGestureHIDType, kHIDEventTypeDockSwipe);
        CGEventSetIntegerValueField(event.as_ptr(), kFieldGesturePhase, phase);
        CGEventSetIntegerValueField(event.as_ptr(), kFieldSwipeMotion, motion);
        CGEventSetIntegerValueField(event.as_ptr(), kFieldGesturePhaseAlias, phase);
        CGEventSetDoubleValueField(event.as_ptr(), kFieldSwipeProgress, progress);
        CGEventSetDoubleValueField(event.as_ptr(), kFieldZoomDeltaY, 3.0);
        CGEventSetDoubleValueField(event.as_ptr(), kFieldSourceTimestamp, mach_absolute_time() as f64);
        let (position, velocity_field) = if horizontal {
            (kFieldSwipePositionX, kFieldSwipeVelocityX)
        } else {
            (kFieldSwipePositionY, kFieldSwipeVelocityY)
        };
        CGEventSetDoubleValueField(event.as_ptr(), position, 0.1);
        if velocity != 0.0 {
            CGEventSetDoubleValueField(event.as_ptr(), velocity_field, velocity);
        }
    }
    attach_payload(&event)
}

// macOS 27 rejects synthetic dock swipes unless each carries a serialized
// raw IOHID queue element in field 4205. The layout must match byte for byte.
const HEADER_LEN: usize = 28;
const FLUID_LEN: usize = 40;
const VELOCITY_LEN: usize = 28;
const IOHID_TYPE_FLUID_TOUCH_GESTURE: u32 = 23;
const IOHID_TYPE_VELOCITY: u32 = 9;
const IOHID_FLAVOR_DOCK_PRIMARY: u16 = 3;
const SERIALIZED_VERSION: [u8; 4] = [0, 0, 0, 2];

fn fixed_1616(value: f64) -> i32 {
    let fixed = (value * 65536.0) as i32;
    if fixed == 0 && value != 0.0 {
        return if value > 0.0 { 1 } else { -1 };
    }
    fixed
}

/// The raw IOHID queue element for a dock event, laid out as iss does:
/// a header, a fluid touch gesture record and, when moving or ending, a
/// velocity record. Fields are little endian and packed.
fn iohid_payload(event: &Event) -> Vec<u8> {
    // SAFETY: event is live
    let (phase, motion, progress, position_x, position_y, velocity_x, velocity_y, timestamp) = unsafe {
        let e = event.as_ptr();
        (
            CGEventGetIntegerValueField(e, kFieldGesturePhase),
            CGEventGetIntegerValueField(e, kFieldSwipeMotion),
            CGEventGetDoubleValueField(e, kFieldSwipeProgress),
            CGEventGetDoubleValueField(e, kFieldSwipePositionX),
            CGEventGetDoubleValueField(e, kFieldSwipePositionY),
            CGEventGetDoubleValueField(e, kFieldSwipeVelocityX),
            CGEventGetDoubleValueField(e, kFieldSwipeVelocityY),
            CGEventGetTimestamp(e),
        )
    };
    let with_velocity = velocity_x != 0.0 || velocity_y != 0.0 || phase == PHASE_ENDED;

    let mut payload = Vec::with_capacity(HEADER_LEN + FLUID_LEN + VELOCITY_LEN);
    // header: timestamp, sender id, options, attribute length, event count
    // SAFETY: plain call
    let timestamp = if timestamp != 0 {
        timestamp
    } else {
        unsafe { mach_absolute_time() }
    };
    payload.extend(timestamp.to_le_bytes());
    payload.extend(0u64.to_le_bytes());
    payload.extend(0u32.to_le_bytes());
    payload.extend(0u32.to_le_bytes());
    payload.extend((if with_velocity { 2u32 } else { 1 }).to_le_bytes());

    // fluid touch gesture: event base (size, type, options, depth, reserved), then fields
    payload.extend((FLUID_LEN as u32).to_le_bytes());
    payload.extend(IOHID_TYPE_FLUID_TOUCH_GESTURE.to_le_bytes());
    payload.extend((((phase & 0xFF) as u32) << 24).to_le_bytes());
    payload.extend([0u8; 4]);
    payload.extend(fixed_1616(position_x).to_le_bytes());
    payload.extend(fixed_1616(position_y).to_le_bytes());
    payload.extend(0i32.to_le_bytes());
    payload.extend(0u32.to_le_bytes()); // swipe mask
    payload.extend((motion as u16).to_le_bytes());
    payload.extend(IOHID_FLAVOR_DOCK_PRIMARY.to_le_bytes());
    payload.extend(fixed_1616(progress).to_le_bytes());

    if with_velocity {
        payload.extend((VELOCITY_LEN as u32).to_le_bytes());
        payload.extend(IOHID_TYPE_VELOCITY.to_le_bytes());
        payload.extend(0u32.to_le_bytes());
        payload.extend([1u8, 0, 0, 0]); // depth 1
        payload.extend(fixed_1616(velocity_x).to_le_bytes());
        payload.extend(fixed_1616(velocity_y).to_le_bytes());
        payload.extend(0i32.to_le_bytes());
    }
    payload
}

// A copy of `event` with the IOHID payload appended to its serialized form.
fn attach_payload(event: &Event) -> Option<Event> {
    // SAFETY: the data objects are created and released here; event is live
    unsafe {
        let data = CGEventCreateData(std::ptr::null(), event.as_ptr());
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize);
        if !bytes.starts_with(&SERIALIZED_VERSION) {
            CFRelease(data);
            return None;
        }

        let payload = iohid_payload(event);
        let mut augmented = bytes.to_vec();
        CFRelease(data);
        augmented.extend((payload.len() as u16).to_be_bytes());
        augmented.extend((kFieldRawIOHIDPayload as u16).to_be_bytes());
        augmented.extend(payload);

        let data = CFDataCreate(std::ptr::null(), augmented.as_ptr(), augmented.len() as isize);
        if data.is_null() {
            return None;
        }
        let created = CGEventCreateFromData(std::ptr::null(), data);
        CFRelease(data);
        (!created.is_null()).then_some(Event(created))
    }
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

// CFData, for serialized events
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    fn CFDataGetLength(data: *const c_void) -> isize;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swipe::{SwipeDetector, SwipeDirection, SwipePhase};

    fn step(axis: SwipeAxis, phase: SwipePhase, progress: f64, velocity: f64) -> SwipeStep {
        SwipeStep {
            axis,
            phase,
            progress,
            velocity,
        }
    }

    fn axis(direction: SwipeDirection) -> SwipeAxis {
        if direction.is_horizontal() {
            SwipeAxis::Horizontal
        } else {
            SwipeAxis::Vertical
        }
    }

    // A swipe as a real trackpad reports it, in the real sign convention.
    fn real_swipe(direction: SwipeDirection) -> Vec<SwipeStep> {
        let (axis, sign) = (axis(direction), direction.sign());
        vec![
            step(axis, SwipePhase::Began, 0.0, 0.0),
            step(axis, SwipePhase::Changed, sign * 0.3, 0.0),
            step(axis, SwipePhase::Ended, sign * 1.1, sign * 5.7),
        ]
    }

    #[test]
    fn payload_layout_matches_iss() {
        // iss static_asserts: header 28, fluid gesture 40, velocity 28 bytes
        let (began, _) = step_events(&step(SwipeAxis::Horizontal, SwipePhase::Began, 0.0, 0.0)).unwrap();
        assert_eq!(iohid_payload(&began).len(), HEADER_LEN + FLUID_LEN);
        let (ended, _) = step_events(&step(SwipeAxis::Horizontal, SwipePhase::Ended, 1.0, 4.0)).unwrap();
        assert_eq!(iohid_payload(&ended).len(), HEADER_LEN + FLUID_LEN + VELOCITY_LEN);
    }

    #[test]
    fn synthetic_signs_match_hardware_measurements() {
        // measured on macOS 27: synthetic positive progress goes to the Space
        // on the left and opens Mission Control, and real swipes left and up
        // report negative progress
        let replayed = |direction: SwipeDirection| {
            let real = step(axis(direction), SwipePhase::Changed, direction.sign() * 0.5, 0.0);
            let (dock, _) = step_events(&real).unwrap();
            // SAFETY: event is live
            unsafe { CGEventGetDoubleValueField(dock.as_ptr(), kFieldSwipeProgress) }
        };
        assert_eq!(replayed(SwipeDirection::Left), 0.5);
        assert_eq!(replayed(SwipeDirection::Right), -0.5);
        assert_eq!(replayed(SwipeDirection::Up), 0.5);
        assert_eq!(replayed(SwipeDirection::Down), -0.5);
    }

    #[test]
    fn replayed_swipes_read_back_reversed() {
        // the detector reads real swipes, whose sign is the opposite of what
        // synthesis needs on both axes
        for (direction, read_back) in [
            (SwipeDirection::Left, SwipeDirection::Right),
            (SwipeDirection::Right, SwipeDirection::Left),
            (SwipeDirection::Up, SwipeDirection::Down),
            (SwipeDirection::Down, SwipeDirection::Up),
        ] {
            let mut detector = SwipeDetector::default();
            let seen: Vec<_> = real_swipe(direction)
                .iter()
                .map(|real| step_events(real).unwrap().0)
                .filter_map(|event| dock_event(event.as_ptr()))
                .filter_map(|dock| detector.feed(&dock))
                .collect();
            assert_eq!(seen, [read_back], "{direction:?}");
        }
    }

    #[test]
    fn replay_keeps_the_phase_and_the_velocity_axis() {
        let ended = step(SwipeAxis::Vertical, SwipePhase::Ended, -0.9, -4.5);
        let (dock, companion) = step_events(&ended).unwrap();
        let read = dock_event(dock.as_ptr()).unwrap();
        assert_eq!((read.motion, read.phase), (MOTION_VERTICAL, PHASE_ENDED));
        assert_eq!((read.velocity_x, read.velocity_y), (0.0, 4.5));
        assert!(is_gesture_event(companion.as_ptr()));
    }

    #[test]
    fn every_replayed_step_carries_the_payload() {
        for real in real_swipe(SwipeDirection::Up) {
            assert!(has_payload(&step_events(&real).unwrap().0), "missing payload");
        }
        // a plain event does not, so the check can tell the difference
        assert!(!has_payload(&new_event().unwrap()));
    }

    #[test]
    fn only_gesture_events_are_recognized_as_such() {
        let companion = companion_event().unwrap();
        assert!(is_gesture_event(companion.as_ptr()));
        assert!(dock_event(companion.as_ptr()).is_none());

        let plain = new_event().unwrap();
        assert!(!is_gesture_event(plain.as_ptr()));
        assert!(dock_event(plain.as_ptr()).is_none());
    }

    unsafe extern "C" {
        fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, name: *const std::ffi::c_char) -> *mut c_void;
    }

    // Resolved at run time: these are only in the dyld shared cache.
    fn active_space() -> u64 {
        // SAFETY: symbols resolved from SkyLight with their known signatures
        unsafe {
            let sky = dlopen(
                c"/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight".as_ptr(),
                2,
            );
            let main: extern "C" fn() -> i32 = std::mem::transmute(dlsym(sky, c"CGSMainConnectionID".as_ptr()));
            let active: extern "C" fn(i32) -> u64 = std::mem::transmute(dlsym(sky, c"CGSGetActiveSpace".as_ptr()));
            active(main())
        }
    }

    // Replay a swipe to the right through the posting thread, from `path` of
    // progress values, and report whether the Space changed.
    fn swipe_right(path: &[f64], end: SwipePhase, velocity: f64) -> bool {
        let before = active_space();
        let horizontal = |phase, progress, velocity| step(SwipeAxis::Horizontal, phase, progress, velocity);
        replay(horizontal(SwipePhase::Began, 0.0, 0.0));
        for &progress in path {
            replay(horizontal(SwipePhase::Changed, progress, 0.0));
            std::thread::sleep(crate::swipe::STEP_SPACING);
        }
        replay(horizontal(end, *path.last().unwrap_or(&0.0), velocity));
        std::thread::sleep(std::time::Duration::from_millis(1500));
        active_space() != before
    }

    fn ramp(from: f64, to: f64, steps: usize) -> Vec<f64> {
        (1..=steps)
            .map(|i| from + (to - from) * i as f64 / steps as f64)
            .collect()
    }

    /// The Dock follows replayed progress rather than jumping: a swipe
    /// pulled back or cancelled stays put, one pushed through switches.
    /// Needs macOS 27, a Space to the right of the current one, and
    /// Accessibility for the test binary; it switches Spaces while it runs.
    #[test]
    #[ignore = "switches Spaces on this Mac; run by hand"]
    fn the_dock_follows_a_replayed_swipe() {
        let home = active_space();
        let back = || {
            let left = |phase, progress: f64, velocity: f64| step(SwipeAxis::Horizontal, phase, -progress, -velocity);
            for real in [
                left(SwipePhase::Began, 0.0, 0.0),
                left(SwipePhase::Changed, 1.0, 0.0),
                left(SwipePhase::Ended, 1.0, 5.0),
            ] {
                replay(real);
            }
            std::thread::sleep(std::time::Duration::from_millis(1500));
        };

        assert!(
            swipe_right(&ramp(0.0, 1.0, 20), SwipePhase::Ended, 5.0),
            "a full swipe should switch"
        );
        back();
        assert_eq!(active_space(), home, "could not get back to the starting Space");

        let out_and_back = [ramp(0.0, 0.6, 20), ramp(0.6, 0.05, 20)].concat();
        assert!(
            !swipe_right(&out_and_back, SwipePhase::Ended, 0.0),
            "a pulled back swipe should stay"
        );
        assert!(
            !swipe_right(&ramp(0.0, 0.6, 20), SwipePhase::Cancelled, 0.0),
            "a cancelled swipe should stay"
        );
        assert_eq!(active_space(), home);
    }

    // The serialized event carries field 4205: a big-endian length, the
    // tag, then the payload.
    fn has_payload(event: &Event) -> bool {
        // SAFETY: data is created and released here
        unsafe {
            let data = CGEventCreateData(std::ptr::null(), event.as_ptr());
            let bytes = std::slice::from_raw_parts(CFDataGetBytePtr(data), CFDataGetLength(data) as usize);
            let found = bytes
                .windows(2)
                .any(|window| window == (kFieldRawIOHIDPayload as u16).to_be_bytes());
            CFRelease(data);
            found
        }
    }
}
