//! Native modal-menu regression, executed by the main-thread test harness.

pub(super) fn run() {
    use objc2::{MainThreadOnly, sel};
    use objc2_app_kit::{NSApplication, NSMenuItem, NSWindow};
    use objc2_foundation::{MainThreadMarker, NSString};
    let mtm = MainThreadMarker::new().expect("test runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let home = tempfile::tempdir().unwrap();
    let controller = crate::controller::spawn(home.path().to_owned(), "Local system".to_owned()).unwrap();
    let target = super::AppDelegate::new(mtm, controller);
    let root = super::menu::new("Daisy", mtm);
    let peers = super::menu::new("Paired Peers", mtm);
    let peer = super::menu::new("Peer", mtm);
    // SAFETY: the test retains the window for the whole modal session.
    let window = unsafe { NSWindow::new(mtm) };
    let session = app.beginModalSessionForWindow(&window);
    // SAFETY: session is live and belongs to this application.
    unsafe { app.runModalSession(session) };

    for menu in [&root, &peers, &peer] {
        // SAFETY: the action is implemented by the retained target below.
        let action = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                &NSString::from_str("Action"),
                Some(sel!(openDaisy:)),
                &NSString::from_str(""),
            )
        };
        // SAFETY: target implements openDaisy: and outlives the menus.
        unsafe { action.setTarget(Some(&target)) };
        menu.addItem(&action);
        action.setEnabled(true);
        menu.update();
        assert!(action.isEnabled(), "available actions must stay enabled");
        // SAFETY: Daisy implements openDaisy: with an object sender.
        assert!(unsafe { app.sendAction_to_from(sel!(openDaisy:), Some(&target), Some(&action)) });
        action.setEnabled(false);
        menu.update();
        assert!(!action.isEnabled(), "unavailable actions must stay disabled");
    }
    // SAFETY: session was begun above and has not been ended.
    unsafe { app.endModalSession(session) };
}
