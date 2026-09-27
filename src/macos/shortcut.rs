//! Replaying swipes as Mission Control keyboard shortcuts.
//!
//! Used on Macs before macOS 27, where the synthesized swipe events are not
//! good enough: they switch Spaces without animation and cannot open Mission
//! Control. The shortcuts are public, animated like a real swipe, and read
//! from this Mac's own keyboard settings so custom bindings still work.

use std::sync::OnceLock;

use plist::Value;

use super::ffi::*;
use crate::swipe::SwipeDirection;

/// A key and the modifier flags to hold with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub key: u16,
    pub flags: u64,
}

/// What this Mac's settings say about one shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    Enabled(Shortcut),
    Disabled,
}

// Control plus the secondary function flag arrow keys always carry, as the
// settings store them.
const CONTROL_ARROW: u64 = 0x0004_0000 | 0x0080_0000;
const CONTROL_FLAG: u64 = 0x0004_0000;
const CONTROL_KEY: u16 = 59;

/// The symbolic hotkey ID in `com.apple.symbolichotkeys`, and the key macOS
/// binds it to by default.
fn hotkey(direction: SwipeDirection) -> (&'static str, u16) {
    match direction {
        SwipeDirection::Left => ("79", 123),  // Move left a space
        SwipeDirection::Right => ("81", 124), // Move right a space
        SwipeDirection::Up => ("32", 126),    // Mission Control
        SwipeDirection::Down => ("33", 125),  // Application windows
    }
}

/// Look up the shortcut for `direction` in the `AppleSymbolicHotKeys`
/// dictionary. An absent entry, or one without a binding, means the macOS
/// default.
pub fn setting(hotkeys: Option<&Value>, direction: SwipeDirection) -> Setting {
    let (id, default_key) = hotkey(direction);
    let default = Shortcut {
        key: default_key,
        flags: CONTROL_ARROW,
    };
    let Some(entry) = hotkeys
        .and_then(Value::as_dictionary)
        .and_then(|hotkeys| hotkeys.get(id))
    else {
        return Setting::Enabled(default);
    };
    let entry = entry.as_dictionary();
    let enabled = entry
        .and_then(|entry| entry.get("enabled"))
        .and_then(Value::as_boolean)
        .unwrap_or(true);
    if !enabled {
        return Setting::Disabled;
    }
    // parameters are [character, key code, modifier flags]
    let parameters = entry
        .and_then(|entry| entry.get("value"))
        .and_then(Value::as_dictionary)
        .and_then(|value| value.get("parameters"))
        .and_then(Value::as_array);
    let custom = parameters.and_then(|parameters| {
        let key = parameters.get(1)?.as_unsigned_integer()?;
        let flags = parameters.get(2)?.as_unsigned_integer()?;
        Some(Shortcut {
            key: u16::try_from(key).ok()?,
            flags,
        })
    });
    Setting::Enabled(custom.unwrap_or(default))
}

/// This Mac's symbolic hotkey settings, read once through `defaults` so
/// they come from the preferences cache rather than a possibly stale file.
fn hotkeys() -> Option<&'static Value> {
    static HOTKEYS: OnceLock<Option<Value>> = OnceLock::new();
    HOTKEYS
        .get_or_init(|| {
            let output = std::process::Command::new("/usr/bin/defaults")
                .args(["export", "com.apple.symbolichotkeys", "-"])
                .output()
                .ok()?;
            let settings = Value::from_reader_xml(output.stdout.as_slice()).ok()?;
            settings.as_dictionary()?.get("AppleSymbolicHotKeys").cloned()
        })
        .as_ref()
}

/// Replay `direction` as its Mission Control shortcut.
pub fn post(direction: SwipeDirection) {
    let shortcut = match setting(hotkeys(), direction) {
        Setting::Enabled(shortcut) => shortcut,
        Setting::Disabled => {
            tracing::warn!(
                ?direction,
                "the Mission Control shortcut for this swipe is turned off in Keyboard Shortcuts"
            );
            return;
        }
    };
    tracing::debug!(?direction, ?shortcut, "replaying swipe as a shortcut");

    // typed the way a person would: modifier down, key, modifier up
    let holds_control = shortcut.flags & CONTROL_FLAG != 0;
    if holds_control {
        post_key(CONTROL_KEY, true, CONTROL_FLAG, true);
    }
    post_key(shortcut.key, true, shortcut.flags, false);
    post_key(shortcut.key, false, shortcut.flags, false);
    if holds_control {
        post_key(CONTROL_KEY, false, 0, true);
    }
}

fn post_key(key: u16, down: bool, flags: u64, modifier: bool) {
    // SAFETY: the event is created, posted and released here
    unsafe {
        let event = CGEventCreateKeyboardEvent(std::ptr::null_mut(), key, down);
        if event.is_null() {
            return;
        }
        if modifier {
            CGEventSetType(event, kCGEventFlagsChanged);
        }
        CGEventSetFlags(event, flags);
        CGEventPost(kCGHIDEventTap, event);
        CFRelease(event.cast_const());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(xml: &str) -> Value {
        Value::from_reader_xml(xml.as_bytes()).unwrap()
    }

    fn hotkeys(entries: &str) -> Value {
        parse(&format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
            <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
            <plist version="1.0"><dict>{entries}</dict></plist>"#
        ))
    }

    #[test]
    fn missing_settings_use_the_defaults() {
        assert_eq!(
            setting(None, SwipeDirection::Up),
            Setting::Enabled(Shortcut {
                key: 126,
                flags: CONTROL_ARROW
            })
        );
        // an empty dictionary, as on a Mac that never touched these shortcuts
        let empty = hotkeys("");
        assert_eq!(
            setting(Some(&empty), SwipeDirection::Left),
            Setting::Enabled(Shortcut {
                key: 123,
                flags: CONTROL_ARROW
            })
        );
    }

    #[test]
    fn enabled_without_a_binding_means_the_default_key() {
        // exactly what this Mac's settings hold for the Space shortcuts
        let settings = hotkeys("<key>81</key><dict><key>enabled</key><true/></dict>");
        assert_eq!(
            setting(Some(&settings), SwipeDirection::Right),
            Setting::Enabled(Shortcut {
                key: 124,
                flags: CONTROL_ARROW
            })
        );
    }

    #[test]
    fn disabled_shortcuts_are_reported() {
        let settings = hotkeys("<key>32</key><dict><key>enabled</key><false/></dict>");
        assert_eq!(setting(Some(&settings), SwipeDirection::Up), Setting::Disabled);
        // other shortcuts are unaffected
        assert!(matches!(
            setting(Some(&settings), SwipeDirection::Down),
            Setting::Enabled(_)
        ));
    }

    #[test]
    fn custom_bindings_are_used() {
        // Mission Control rebound to Option-F3
        let settings = hotkeys(
            "<key>32</key><dict>
                <key>enabled</key><true/>
                <key>value</key><dict>
                    <key>parameters</key><array>
                        <integer>65535</integer><integer>99</integer><integer>524288</integer>
                    </array>
                    <key>type</key><string>standard</string>
                </dict>
            </dict>",
        );
        assert_eq!(
            setting(Some(&settings), SwipeDirection::Up),
            Setting::Enabled(Shortcut {
                key: 99,
                flags: 524_288
            })
        );
    }

    #[test]
    fn malformed_bindings_fall_back_to_the_default() {
        let settings = hotkeys(
            "<key>79</key><dict>
                <key>enabled</key><true/>
                <key>value</key><dict><key>parameters</key><array><integer>1</integer></array></dict>
            </dict>",
        );
        assert_eq!(
            setting(Some(&settings), SwipeDirection::Left),
            Setting::Enabled(Shortcut {
                key: 123,
                flags: CONTROL_ARROW
            })
        );
    }

    #[test]
    fn reads_this_macs_settings() {
        // whatever this Mac has, every direction resolves to something
        for direction in [
            SwipeDirection::Left,
            SwipeDirection::Right,
            SwipeDirection::Up,
            SwipeDirection::Down,
        ] {
            let _ = setting(super::hotkeys(), direction);
        }
    }
}
