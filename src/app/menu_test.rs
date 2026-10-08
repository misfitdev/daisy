//! Native modal-menu regression, executed by the main-thread test harness.

pub(super) fn run() {
    use objc2::{DefinedClass, MainThreadOnly, sel};
    use objc2_app_kit::{NSApplication, NSMenuItem, NSWindow};
    use objc2_foundation::{MainThreadMarker, NSString};
    let mtm = MainThreadMarker::new().expect("test runs on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    let home = tempfile::tempdir().unwrap();
    let controller = crate::controller::spawn(home.path().to_owned(), "Local system".to_owned()).unwrap();
    let target = super::AppDelegate::new(mtm, controller);
    let add = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str("Add System"),
            Some(sel!(addSystem:)),
            &NSString::from_str(""),
        )
    };
    target.ivars().menu_add.set(add.clone()).unwrap();
    target.ivars().settings.borrow_mut().last_session.connection = crate::controller::Connection::Connect {
        address: "192.168.1.20".to_owned(),
        peer: None,
    };
    *target.ivars().status.borrow_mut() = crate::controller::Status::Connected { peers: Vec::new() };
    target.update_action_buttons();
    assert!(add.isEnabled(), "address connections must still allow adding systems");
    *target.ivars().status.borrow_mut() = crate::controller::Status::Idle;

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

    peer_row_dialogs_are_sheets(mtm, &target);
}

/// An app-modal alert from a peer row can end up off screen while it holds
/// the main thread, so the row's dialogs must attach to its window instead.
fn peer_row_dialogs_are_sheets(mtm: objc2_foundation::MainThreadMarker, target: &super::AppDelegate) {
    use objc2::{DefinedClass, sel};
    use objc2_app_kit::{NSApplication, NSButton, NSWindow};
    use objc2_foundation::NSString;

    *target.ivars().peers.borrow_mut() = vec![crate::peers::Peer {
        name: "Studio".to_owned(),
        key: crate::identity::PublicKey::from_bytes(&[2; 32]).unwrap(),
        policy: crate::trust::Policy::IDLE,
        paired_at: 0,
        last_seen: 0,
        side: crate::input::Side::Right,
        side_chosen: 0,
        signing: None,
        introduced_by: None,
    }];
    for action in [sel!(changeTrust:), sel!(forgetPeer:)] {
        // SAFETY: the window is retained until the end of this iteration.
        let window = unsafe { NSWindow::new(mtm) };
        // SAFETY: Daisy implements both actions with a button sender.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Row"), Some(target), Some(action), mtm)
        };
        button.setTag(0);
        window.contentView().unwrap().addSubview(&button);
        // SAFETY: the action's sender is a button, as Daisy expects.
        let sent =
            unsafe { NSApplication::sharedApplication(mtm).sendAction_to_from(action, Some(target), Some(&button)) };
        assert!(sent, "{action:?} must reach Daisy");
        let sheet = window.attachedSheet();
        assert!(sheet.is_some(), "{action:?} must ask on a sheet over the row's window");
        window.endSheet(&sheet.unwrap());
    }
}
