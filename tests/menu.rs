//! AppKit requires the main thread, so this test has its own harness.

#[cfg(target_os = "macos")]
pub use daisy::{
    COMMIT, control, controller, identity, input, install, layout, macos, pairing, peers, permissions, service, setup,
    share, trust, update,
};

#[cfg(target_os = "macos")]
#[path = "../src/app.rs"]
// This harness exercises the delegate without starting the application run loop.
#[allow(dead_code)]
mod app;

#[cfg(target_os = "macos")]
fn main() {
    app::test_modal_menu_actions();
}

#[cfg(not(target_os = "macos"))]
fn main() {}
