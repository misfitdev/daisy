//! macOS privacy permissions that sharing input needs.
//!
//! macOS grants these to the app's signed identity, and checks them against
//! the process responsible for the caller. Run from Terminal, even from
//! inside Daisy.app, the answer is Terminal's; launch the app with
//! `open` for its own.

use std::ffi::c_void;

/// Status of one permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Granted,
    Denied,
    /// Never asked for; requesting it shows the system prompt.
    Undetermined,
}

/// Accessibility: needed to post keyboard and mouse events.
pub fn accessibility() -> Access {
    // SAFETY: takes no arguments and only reads this process's status
    if unsafe { AXIsProcessTrusted() } {
        Access::Granted
    } else {
        Access::Denied
    }
}

/// Input Monitoring: needed to read the keyboard and mouse.
pub fn input_monitoring() -> Access {
    // SAFETY: plain value in, plain value out
    match unsafe { IOHIDCheckAccess(IOHID_REQUEST_LISTEN_EVENT) } {
        IOHID_ACCESS_GRANTED => Access::Granted,
        IOHID_ACCESS_DENIED => Access::Denied,
        _ => Access::Undetermined,
    }
}

/// Ask macOS to prompt for Accessibility if it is not granted.
pub fn request_accessibility() -> Access {
    // SAFETY: the dictionary is built from CoreFoundation's own constant key,
    // value and callbacks, and released after use
    let trusted = unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &raw const kCFTypeDictionaryKeyCallBacks as *const c_void,
            &raw const kCFTypeDictionaryValueCallBacks as *const c_void,
        );
        let trusted = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        trusted
    };
    if trusted { Access::Granted } else { Access::Denied }
}

/// Ask macOS to prompt for Input Monitoring if it has not been decided.
pub fn request_input_monitoring() -> Access {
    // SAFETY: plain value in, plain value out
    unsafe { IOHIDRequestAccess(IOHID_REQUEST_LISTEN_EVENT) };
    input_monitoring()
}

// IOHIDRequestType and IOHIDAccessType from IOKit/hid/IOHIDLib.h
const IOHID_REQUEST_LISTEN_EVENT: u32 = 1;
const IOHID_ACCESS_GRANTED: u32 = 0;
const IOHID_ACCESS_DENIED: u32 = 1;

// Only ever used by address, as CoreFoundation's documented callback tables.
#[repr(C)]
struct Opaque {
    _private: [u8; 0],
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    static kAXTrustedCheckOptionPrompt: *const c_void;
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
    fn IOHIDRequestAccess(request: u32) -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: Opaque;
    static kCFTypeDictionaryValueCallBacks: Opaque;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_queries_do_not_crash() {
        // the answers depend on this machine's settings; only the calls are checked
        let _ = accessibility();
        let _ = input_monitoring();
    }
}
