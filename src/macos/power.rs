//! Keeping this system awake while it belongs to a group, waking its
//! display when control arrives, and telling whether its screen is locked.

use std::ffi::{CStr, c_void};

use anyhow::{Result, bail};

use super::ffi::{
    CFBooleanGetTypeID, CFBooleanGetValue, CFGetTypeID, CFRelease, CFStringCreateWithCString, kCFStringEncodingUTF8,
};

/// Holds off idle system sleep while it lives; displays may still dim and
/// sleep. Released on drop.
pub struct KeepAwake(u32);

impl KeepAwake {
    pub fn new(reason: &CStr) -> Result<Self> {
        let kind = CfString::new(c"PreventUserIdleSystemSleep");
        let name = CfString::new(reason);
        let mut id = 0;
        // SAFETY: both strings live until the call returns; id is a valid out-pointer
        let result = unsafe { IOPMAssertionCreateWithName(kind.0, ASSERTION_LEVEL_ON, name.0, &mut id) };
        if result != 0 {
            bail!("macOS refused to keep this system awake (IOKit error {result:#x})");
        }
        Ok(Self(id))
    }

    /// Whether macOS still holds this assertion.
    #[cfg(test)]
    fn held(&self) -> bool {
        held(self.0)
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        // SAFETY: the id came from IOPMAssertionCreateWithName and is released once
        unsafe { IOPMAssertionRelease(self.0) };
    }
}

#[cfg(test)]
fn held(id: u32) -> bool {
    // SAFETY: a plain id; the copied dictionary is released here
    unsafe {
        let properties = IOPMAssertionCopyProperties(id);
        if properties.is_null() {
            return false;
        }
        CFRelease(properties);
        true
    }
}

/// Wakes the display, as a person touching this system would, when control
/// arrives here. Does not change any setting.
pub fn declare_activity() {
    let name = CfString::new(c"Daisy control arrived");
    let mut id = 0;
    // SAFETY: the string lives until the call returns; id is a valid out-pointer
    let result = unsafe { IOPMAssertionDeclareUserActivity(name.0, USER_ACTIVE_LOCAL, &mut id) };
    if result == 0 {
        // SAFETY: the id came from the call above and is released once
        unsafe { IOPMAssertionRelease(id) };
    }
}

/// Whether this system's screen is locked. Posted input does not reach the
/// lock screen, so a person must unlock it here.
pub fn screen_locked() -> bool {
    // SAFETY: the dictionary is released here, and the value it holds is
    // only read while it lives
    unsafe {
        let session = CGSessionCopyCurrentDictionary();
        if session.is_null() {
            return false;
        }
        let key = CfString::new(c"CGSSessionScreenIsLocked");
        let value = CFDictionaryGetValue(session, key.0);
        let locked = !value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID() && CFBooleanGetValue(value);
        CFRelease(session);
        locked
    }
}

struct CfString(*const c_void);

impl CfString {
    fn new(text: &CStr) -> Self {
        // SAFETY: text is NUL-terminated UTF-8
        Self(unsafe { CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), kCFStringEncodingUTF8) })
    }
}

impl Drop for CfString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: created above and released once
            unsafe { CFRelease(self.0) };
        }
    }
}

// IOPMAssertionLevel and IOPMUserActiveType, from IOKit/pwr_mgt/IOPMLib.h
const ASSERTION_LEVEL_ON: u32 = 255;
const USER_ACTIVE_LOCAL: u32 = 0;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPMAssertionCreateWithName(kind: *const c_void, level: u32, name: *const c_void, id: *mut u32) -> i32;
    fn IOPMAssertionRelease(id: u32) -> i32;
    fn IOPMAssertionDeclareUserActivity(name: *const c_void, kind: u32, id: *mut u32) -> i32;
    #[cfg(test)]
    fn IOPMAssertionCopyProperties(id: u32) -> *const c_void;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGSessionCopyCurrentDictionary() -> *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryGetValue(dictionary: *const c_void, key: *const c_void) -> *const c_void;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assertion_is_held_while_it_lives_and_released_after() {
        let awake = KeepAwake::new(c"Daisy test").unwrap();
        let id = awake.0;
        assert!(awake.held());
        drop(awake);
        assert!(!held(id));
    }

    #[test]
    fn the_lock_state_is_read_without_crashing() {
        // this system is in use while tests run, so it is not locked
        assert!(!screen_locked());
    }
}
