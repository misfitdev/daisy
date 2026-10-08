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

    peer_row_dialogs_are_sheets(mtm, &target, home.path());
}

/// An app-modal alert from a peer row can end up off screen while it holds
/// the main thread, so the row's dialogs must attach to its window, and
/// confirming them must still reach the controller.
fn peer_row_dialogs_are_sheets(
    mtm: objc2_foundation::MainThreadMarker,
    target: &super::AppDelegate,
    home: &std::path::Path,
) {
    use objc2::{DefinedClass, sel};
    use objc2_app_kit::{NSApplication, NSButton, NSWindow};
    use objc2_foundation::NSString;

    let key = crate::identity::PublicKey::from_bytes(&[2; 32]).unwrap();
    let now = crate::trust::now();
    let peers_file = home.join("trust-v5/peers.toml");
    std::fs::create_dir_all(peers_file.parent().unwrap()).unwrap();
    std::fs::write(
        &peers_file,
        format!(
            "[[peer]]\nkey = \"{}\"\nname = \"Studio\"\ntrust = \"idle\"\npaired_at = {now}\nlast_seen = {now}\nside = \"Right\"\nside_chosen = 0\n",
            key.to_hex()
        ),
    )
    .unwrap();
    *target.ivars().peers.borrow_mut() = vec![crate::peers::Peer {
        name: "Studio".to_owned(),
        key,
        policy: crate::trust::Policy::IDLE,
        paired_at: now,
        last_seen: now,
        side: crate::input::Side::Right,
        side_chosen: 0,
        signing: None,
        introduced_by: None,
    }];

    let open = |action| {
        // SAFETY: the window is retained by the caller until its sheet ends.
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
        let sheet = window
            .attachedSheet()
            .unwrap_or_else(|| panic!("{action:?} must ask on a sheet over the row's window"));
        (window, sheet)
    };

    let (_window, sheet) = open(sel!(changeTrust:));
    click(&sheet, "Forever");
    click(&sheet, "Save");
    wait_for("Save to change the peer's trust", || {
        std::fs::read_to_string(&peers_file).is_ok_and(|text| text.contains("trust = \"forever\""))
    });

    let main = super::window::MainViews::new(mtm, target);
    target
        .ivars()
        .main
        .set(main)
        .ok()
        .expect("the test sets the main window once");
    // SAFETY: Daisy implements addPeerByAddress: with an optional sender.
    assert!(unsafe {
        NSApplication::sharedApplication(mtm).sendAction_to_from(sel!(addPeerByAddress:), Some(target), None)
    });
    let main = &target.ivars().main.get().unwrap().window;
    let sheet = main
        .attachedSheet()
        .expect("adding a peer must ask on a sheet over the main window");
    click(&sheet, "Connect");
    assert!(
        main.attachedSheet().is_some(),
        "an empty address must keep the sheet open"
    );
    assert!(
        has_text(&sheet.contentView().unwrap(), "Enter a local name or IP address."),
        "an empty address must say what is missing"
    );
    click(&sheet, "Cancel");
    wait_for("Cancel to close the sheet", || main.attachedSheet().is_none());

    let (_window, sheet) = open(sel!(forgetPeer:));
    click(&sheet, "Forget Peer");
    // test builds have no device identity to sign the revocation, so the
    // controller reports the attempt rather than removing the peer
    wait_for("Forget Peer to reach the controller", || {
        std::iter::from_fn(|| target.ivars().controller.try_recv().ok().flatten()).any(|event| match event {
            crate::controller::Event::Peers(peers) => peers.iter().all(|peer| peer.key != key),
            crate::controller::Event::Status(crate::controller::Status::Problem { summary, .. }) => {
                summary == "That peer could not be forgotten"
            }
            _ => false,
        })
    });
}

fn click(sheet: &objc2_app_kit::NSWindow, title: &str) {
    fn find(view: &objc2_app_kit::NSView, title: &str) -> Option<objc2::rc::Retained<objc2_app_kit::NSButton>> {
        view.subviews().iter().find_map(|child| {
            child
                .downcast_ref::<objc2_app_kit::NSButton>()
                .filter(|button| button.title().to_string() == title)
                .map(|button| button.retain())
                .or_else(|| find(&child, title))
        })
    }
    use objc2::Message;
    let button =
        find(&sheet.contentView().unwrap(), title).unwrap_or_else(|| panic!("the sheet has no {title} button"));
    // SAFETY: the button belongs to a live sheet and has no sender requirement.
    unsafe { button.performClick(None) };
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
        objc2_foundation::NSRunLoop::currentRunLoop()
            .runUntilDate(&objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.05));
    }
}

fn has_text(view: &objc2_app_kit::NSView, text: &str) -> bool {
    view.subviews().iter().any(|child| {
        child
            .downcast_ref::<objc2_app_kit::NSTextField>()
            .is_some_and(|field| !field.isHidden() && field.stringValue().to_string() == text)
            || has_text(&child, text)
    })
}
