//! Share one keyboard, mouse and trackpad swipes between systems.

/// The commit this build came from, with `-modified` when the tree had
/// uncommitted changes.
pub const COMMIT: &str = env!("DAISY_COMMIT");

/// The version and the commit it was built from.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("DAISY_COMMIT"), ")");

#[cfg(target_os = "macos")]
pub mod app;
pub mod clipboard;
pub mod control;
pub mod controller;
pub mod discovery;
pub mod identity;
pub mod input;
pub mod install;
pub mod introduce;
pub mod latency;
pub mod launcher;
pub mod layout;
pub mod macos;
pub mod pairing;
pub mod peers;
pub mod permissions;
pub mod protocol;
pub mod reconnect;
pub mod service;
pub mod session;
pub mod setup;
pub mod shake;
pub mod share;
pub mod swipe;
pub mod trust;
