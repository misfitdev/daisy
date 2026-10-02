//! Settings an administrator enforces with a configuration profile for
//! `dev.misfit.daisy`. Only enforced values count, so a person cannot set
//! one with `defaults write`.

use std::ffi::{CStr, c_void};

use super::ffi::{
    CFBooleanGetTypeID, CFBooleanGetValue, CFGetTypeID, CFPreferencesAppValueIsForced, CFPreferencesCopyAppValue,
    CFRelease, CFStringCreateWithCString, kCFStringEncodingUTF8,
};

const DOMAIN: &CStr = c"dev.misfit.daisy";

/// Whether Always Discoverable may be turned on. A profile setting
/// `AllowAlwaysDiscoverable` to false forbids it.
pub fn always_discoverable_allowed() -> bool {
    enforced_bool(c"AllowAlwaysDiscoverable") != Some(false)
}

fn enforced_bool(key: &CStr) -> Option<bool> {
    // SAFETY: every CF object created or copied here is released here, and
    // the value's type is checked before reading it
    unsafe {
        let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), kCFStringEncodingUTF8);
        let domain = CFStringCreateWithCString(std::ptr::null(), DOMAIN.as_ptr(), kCFStringEncodingUTF8);
        let value: *const c_void = if key.is_null() || domain.is_null() || !CFPreferencesAppValueIsForced(key, domain) {
            std::ptr::null()
        } else {
            CFPreferencesCopyAppValue(key, domain)
        };
        let result = (!value.is_null() && CFGetTypeID(value) == CFBooleanGetTypeID()).then(|| CFBooleanGetValue(value));
        for object in [value, key, domain] {
            if !object.is_null() {
                CFRelease(object);
            }
        }
        result
    }
}
