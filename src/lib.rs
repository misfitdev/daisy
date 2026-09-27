//! Share one keyboard, mouse and trackpad swipes between Macs.

#[cfg(target_os = "macos")]
pub mod app;
pub mod clipboard;
pub mod controller;
pub mod identity;
pub mod input;
pub mod launcher;
pub mod macos;
pub mod pairing;
pub mod peers;
pub mod permissions;
pub mod protocol;
pub mod reconnect;
pub mod service;
pub mod session;
pub mod shake;
pub mod share;
pub mod swipe;
pub mod trust;
