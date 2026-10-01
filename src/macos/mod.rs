//! macOS side of input sharing: reading input on the system with the keyboard
//! and replaying it on the other.

pub mod capture;
mod ffi;
pub mod inject;
pub mod install;
pub mod pasteboard;
mod pointer;
pub mod power;
pub mod shortcut;
pub mod swipe;

use anyhow::{Result, bail};

/// Undo a pointer left enlarged by a Daisy that crashed while the
/// pointer was being shaken.
pub use pointer::{main_run_loop_starting, restore as restore_pointer};

use crate::input::Rect;

/// Lets this app change the cursor while another app is in front. A private
/// window-server property; the Dock still controls the cursor over the Dock.
pub(crate) fn set_cursor_in_background() {
    // SAFETY: the string is created and released here; the other calls take plain values
    unsafe {
        let key = ffi::CFStringCreateWithCString(
            std::ptr::null(),
            c"SetsCursorInBackground".as_ptr(),
            ffi::kCFStringEncodingUTF8,
        );
        if !key.is_null() {
            let connection = ffi::_CGSDefaultConnection();
            ffi::CGSSetConnectionProperty(connection, connection, key, ffi::kCFBooleanTrue);
            ffi::CFRelease(key);
        }
    }
}

/// Every active display, in macOS global coordinates.
pub fn displays() -> Result<Vec<Rect>> {
    const MAX_DISPLAYS: usize = 16;
    let mut displays = [0; MAX_DISPLAYS];
    let mut count = 0;
    // SAFETY: the buffer holds MAX_DISPLAYS entries, and count reports how many were written
    let error = unsafe { ffi::CGGetActiveDisplayList(MAX_DISPLAYS as u32, displays.as_mut_ptr(), &mut count) };
    if error != 0 || count == 0 {
        bail!("could not list displays (CoreGraphics error {error})");
    }
    Ok(displays[..count as usize]
        .iter()
        // SAFETY: each id came from CGGetActiveDisplayList
        .map(|&display| unsafe { ffi::CGDisplayBounds(display) })
        .map(|rect| Rect {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        })
        .collect())
}

/// The running macOS major version, or 0 if it cannot be read.
///
/// Read once and cached: the event tap asks on every trackpad gesture event,
/// hundreds of times a second, and must never wait on anything slow.
pub(crate) fn major_version() -> u32 {
    static VERSION: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *VERSION.get_or_init(read_major_version)
}

fn read_major_version() -> u32 {
    let mut buffer = [0u8; 32];
    let mut len = buffer.len();
    // SAFETY: the name is NUL terminated, and len bounds what sysctl writes
    let status = unsafe {
        sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return 0;
    }
    std::str::from_utf8(&buffer[..len])
        .ok()
        .and_then(|version| version.trim_end_matches('\0').split('.').next()?.parse().ok())
        .unwrap_or(0)
}

unsafe extern "C" {
    fn sysctlbyname(
        name: *const std::ffi::c_char,
        old: *mut std::ffi::c_void,
        old_len: *mut usize,
        new: *mut std::ffi::c_void,
        new_len: usize,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_real_display_is_listed() {
        let displays = displays().unwrap();
        assert!(!displays.is_empty());
        // the main display sits at the origin
        assert!(
            displays.iter().any(|display| display.x == 0.0 && display.y == 0.0),
            "{displays:?}"
        );
        assert!(
            displays
                .iter()
                .all(|display| display.width >= 640.0 && display.height >= 480.0),
            "{displays:?}"
        );
    }

    #[test]
    fn reads_the_macos_version() {
        assert!(major_version() >= 26);
        assert_eq!(read_major_version(), major_version());
    }

    #[test]
    fn version_check_is_cheap_enough_for_the_event_tap() {
        // regression: it once started a process per call, lagging the mouse
        let start = std::time::Instant::now();
        for _ in 0..100_000 {
            std::hint::black_box(swipe::can_recognize());
        }
        assert!(
            start.elapsed() < std::time::Duration::from_millis(100),
            "{:?}",
            start.elapsed()
        );
    }
}
