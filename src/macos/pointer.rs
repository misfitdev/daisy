//! Enlarging the pointer when it is shaken, as macOS does for its own mouse.
//!
//! The scale set here is global and survives this process, so it is always
//! put back to the size chosen in Accessibility settings: when magnifying
//! ends, when the injector goes away, and when a new one starts, which
//! repairs a pointer left large by a crash.

use std::ffi::{CStr, c_void};
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::ffi::*;
use crate::shake::{HOLD, Zoom};

const FRAME: Duration = Duration::from_millis(16);
const UNIVERSAL_ACCESS: &CStr = c"com.apple.universalaccess";

/// Magnifies the pointer on request, from its own thread.
pub struct Magnifier {
    shaken: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Magnifier {
    /// A magnifier, or `None` if "Shake mouse pointer to locate" is off.
    pub fn new() -> Option<Self> {
        restore();
        if bool_setting(c"CGDisableCursorLocationMagnification") == Some(true) {
            return None;
        }
        let (shaken, requests) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("daisy-pointer".into())
            .spawn(move || {
                while requests.recv().is_ok() {
                    if !magnify(&requests) {
                        break;
                    }
                }
                restore();
            })
            .ok()?;
        Some(Self {
            shaken: Some(shaken),
            thread: Some(thread),
        })
    }

    /// The pointer is being shaken: grow it, or keep it large.
    pub fn shaken(&self) {
        if let Some(shaken) = &self.shaken {
            let _ = shaken.send(());
        }
    }
}

impl Drop for Magnifier {
    fn drop(&mut self) {
        // closing the channel ends the thread, which restores the pointer
        self.shaken.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

// Grow the pointer while shakes keep arriving, then ease it back. Returns
// false if the magnifier went away meanwhile.
fn magnify(requests: &mpsc::Receiver<()>) -> bool {
    let mut zoom = Zoom::new(usual_size());
    let mut last_shake = Instant::now();
    let mut last_frame = Instant::now();
    loop {
        match requests.recv_timeout(FRAME) {
            Ok(()) => last_shake = Instant::now(),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return false,
        }
        loop {
            match requests.try_recv() {
                Ok(()) => last_shake = Instant::now(),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return false,
            }
        }
        let now = Instant::now();
        let shaking = now - last_shake < HOLD;
        set_scale(zoom.step(shaking, now - last_frame));
        last_frame = now;
        if !shaking && zoom.at_rest() {
            return true;
        }
    }
}

/// Put the pointer back to the size chosen in Accessibility settings.
pub fn restore() {
    let usual = usual_size();
    if scale().is_some_and(|scale| scale != usual) {
        set_scale(usual);
    }
}

fn usual_size() -> f64 {
    number_setting(c"mouseDriverCursorSize")
        .filter(|size| (1.0..=4.0).contains(size))
        .unwrap_or(1.0)
}

fn scale() -> Option<f64> {
    let mut scale = 0.0;
    // SAFETY: writes one float through a valid pointer
    let status = unsafe { CGSGetCursorScale(_CGSDefaultConnection(), &mut scale) };
    (status == 0).then_some(f64::from(scale))
}

fn set_scale(scale: f64) {
    // SAFETY: plain values
    unsafe { CGSSetCursorScale(_CGSDefaultConnection(), scale as f32) };
}

fn number_setting(key: &CStr) -> Option<f64> {
    with_setting(key, |value| {
        let mut number = 0.0_f64;
        // SAFETY: value is a live CF object; its type is checked before reading
        unsafe {
            (CFGetTypeID(value) == CFNumberGetTypeID()
                && CFNumberGetValue(value, kCFNumberDoubleType, (&raw mut number).cast()))
            .then_some(number)
        }
    })
}

fn bool_setting(key: &CStr) -> Option<bool> {
    with_setting(key, |value| {
        // SAFETY: value is a live CF object; its type is checked before reading
        unsafe { (CFGetTypeID(value) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(value)) }
    })
}

// Read `key` from the Accessibility preferences, if set.
fn with_setting<T>(key: &CStr, read: impl FnOnce(*const c_void) -> Option<T>) -> Option<T> {
    // SAFETY: every CF object created or copied here is released here
    unsafe {
        let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), kCFStringEncodingUTF8);
        let application = CFStringCreateWithCString(std::ptr::null(), UNIVERSAL_ACCESS.as_ptr(), kCFStringEncodingUTF8);
        let value = if key.is_null() || application.is_null() {
            std::ptr::null()
        } else {
            CFPreferencesCopyAppValue(key, application)
        };
        let result = if value.is_null() { None } else { read(value) };
        for object in [value, key, application] {
            if !object.is_null() {
                CFRelease(object);
            }
        }
        result
    }
}
