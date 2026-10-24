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
    update_controls_report_and_follow_the_switch(mtm, &target);
    an_update_relaunch_closes_the_advanced_sheet(mtm, &target);
    arrangement_confirmation(mtm);
    form_sheets_wrap_and_grow(mtm, &target);
    retention_edits_keep_deadlines_explicit(mtm, &target);
}

/// macOS refuses to quit an app with a sheet open, so an update's relaunch
/// waited while Advanced, where Check Now lives, was still showing.
fn an_update_relaunch_closes_the_advanced_sheet(mtm: objc2_foundation::MainThreadMarker, target: &super::AppDelegate) {
    use objc2::{DefinedClass, MainThreadOnly};
    use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    let advanced = &target.ivars().advanced.get().expect("Advanced was built above").window;
    // SAFETY: a plain titled window, retained for the rest of the check.
    let main = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(400.0, 300.0)),
            NSWindowStyleMask::Titled,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window stays retained by `main` until the check ends.
    unsafe { main.setReleasedWhenClosed(false) };
    main.beginSheet_completionHandler(advanced, None);
    wait_for("the Advanced sheet to attach", || main.attachedSheet().is_some());

    let resumed = std::cell::Cell::new(false);
    crate::macos::update::relaunch(mtm, || {
        assert!(main.attachedSheet().is_none(), "no sheet may hold up the relaunch");
        resumed.set(true);
    });
    assert!(resumed.get(), "the relaunch goes ahead");
    main.close();
}

fn arrangement_confirmation(mtm: objc2_foundation::MainThreadMarker) {
    use crate::controller::{Command, Event};
    use objc2::DefinedClass;
    let home = tempfile::tempdir().unwrap();
    let me = crate::identity::PublicKey::from_bytes(&[1; 32]).unwrap();
    let peer = crate::identity::PublicKey::from_bytes(&[2; 32]).unwrap();
    let store = crate::peers::PeerStore::open(home.path()).unwrap();
    store
        .pin(peer, "Studio", crate::trust::Policy::Forever, crate::trust::now())
        .unwrap();
    store
        .save_arrangement(&(10, me, vec![(me, (0.0, 0.0)), (peer, (5000.0, 0.0))]))
        .unwrap();
    let handle = crate::controller::spawn_arrangement_preview(home.path().to_owned()).unwrap();
    let target = super::AppDelegate::new(mtm, handle);
    *target.ivars().local_name.borrow_mut() = "Local system".to_owned();
    target
        .ivars()
        .main
        .set(super::window::MainViews::new(mtm, &target))
        .ok()
        .unwrap();
    let drain = || {
        while let Ok(Some(event)) = target.ivars().controller.try_recv() {
            target.handle_event(event);
        }
    };
    let views = target.ivars().main.get().unwrap();
    wait_for("the arrangement question", || {
        drain();
        !views.question.isHidden()
    });
    assert!(views.unreachable.stringValue().to_string().contains("Studio"));
    assert!(views.window.isVisible(), "a changed group brings the map forward");
    click(&views.window, "Looks Right");
    wait_for("Looks Right to record and dismiss", || {
        drain();
        views.question.isHidden() && store.confirmation().is_some()
    });
    let confirmed = store.confirmation().unwrap();
    assert!(
        !views.unreachable.isHidden() && views.unreachable.stringValue().to_string().contains("Studio"),
        "confirming placement must not hide an unreachable peer"
    );
    views.window.close();
    let layout = target.ivars().arranged.borrow().as_ref().unwrap().layout.clone();
    target.handle_event(Event::Arranged(crate::share::ArrangementView {
        unreachable: layout.unreachable(me),
        layout: layout.clone(),
        check: None,
        restored: false,
    }));
    assert!(
        !views.window.isVisible(),
        "an unchanged reconnect leaves the window alone"
    );
    // a drop out of reach lands within it, so it confirms without asking
    target
        .ivars()
        .controller
        .send(Command::Place(peer, (6000.0, 0.0)))
        .unwrap();
    wait_for("a drop out of reach to land within it", || {
        drain();
        store
            .confirmation()
            .is_some_and(|saved| saved.version > confirmed.version)
            && target
                .ivars()
                .arranged
                .borrow()
                .as_ref()
                .is_some_and(|arranged| arranged.unreachable.is_empty())
    });
    assert!(views.question.isHidden());
    assert!(
        views.unreachable.isHidden(),
        "the warning clears when every peer is reachable"
    );
    target.ivars().controller.shutdown().unwrap();
    views.window.close();
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
    target
        .ivars()
        .pairing
        .set(super::panel::Panel::new(mtm, target))
        .ok()
        .expect("the test sets the pairing panel once");
    let main = &target.ivars().main.get().unwrap().window;
    for (status, always) in [
        (crate::controller::Status::Idle, false),
        (crate::controller::Status::Connected { peers: Vec::new() }, true),
    ] {
        *target.ivars().status.borrow_mut() = status;
        target.ivars().settings.borrow_mut().always_discoverable = always;
        // SAFETY: Daisy implements this action with an optional sender.
        assert!(unsafe {
            NSApplication::sharedApplication(mtm).sendAction_to_from(sel!(addPeerByAddress:), Some(target), None)
        });
        let sheet = main
            .attachedSheet()
            .expect("direct address entry must remain available without discovery");
        click(&sheet, "Cancel");
        wait_for("Cancel to close direct address entry", || {
            main.attachedSheet().is_none()
        });
    }
    target.ivars().settings.borrow_mut().always_discoverable = false;
    *target.ivars().status.borrow_mut() = crate::controller::Status::Connected { peers: Vec::new() };
    // SAFETY: Daisy implements addPeerByAddress: with an optional sender.
    assert!(unsafe {
        NSApplication::sharedApplication(mtm).sendAction_to_from(sel!(addPeerByAddress:), Some(target), None)
    });
    let main = &target.ivars().main.get().unwrap().window;
    assert!(
        main.attachedSheet().is_none(),
        "adding a system must start with discovery"
    );
    let pairing = target.ivars().pairing.get().unwrap();
    assert!(matches!(pairing.state(), Some(super::panel::State::Waiting { .. })));
    let escape = objc2_app_kit::NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        objc2_app_kit::NSEventType::KeyDown,
        objc2_foundation::NSPoint::new(0.0, 0.0),
        objc2_app_kit::NSEventModifierFlags::empty(),
        0.0,
        pairing.panel.windowNumber(),
        None,
        &NSString::from_str("\u{1b}"),
        &NSString::from_str("\u{1b}"),
        false,
        53,
    ).unwrap();
    assert!(pairing.panel.contentView().unwrap().performKeyEquivalent(&escape));
    assert!(!pairing.panel.isVisible(), "Escape must dismiss discovery");
    assert!(main.attachedSheet().is_none(), "Escape must not open address entry");
    target.add_a_system();
    click(&pairing.panel, "By Address…");
    assert!(
        !pairing.panel.isVisible(),
        "direct entry must close the discovery panel"
    );
    let sheet = main
        .attachedSheet()
        .expect("By Address must ask on a sheet over the main window");
    assert_wrapped_text_fits(
        &sheet,
        "Enter the peer's local name or IP address for a direct connection.",
    );
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

fn assert_wrapped_text_fits(window: &objc2_app_kit::NSWindow, text: &str) {
    use objc2::MainThreadOnly;
    use objc2_app_kit::{NSTextField, NSView};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    fn find(view: &NSView, text: &str) -> Option<objc2::rc::Retained<NSTextField>> {
        use objc2::Message;
        view.subviews().iter().find_map(|child| {
            child
                .downcast_ref::<NSTextField>()
                .filter(|field| field.stringValue().to_string() == text)
                .map(|field| field.retain())
                .or_else(|| find(&child, text))
        })
    }
    let field = find(&window.contentView().unwrap(), text).expect("the full text is in the sheet");
    let reference = NSTextField::wrappingLabelWithString(&NSString::from_str(text), window.mtm());
    reference.setFont(field.font().as_deref());
    let expected = reference
        .cell()
        .unwrap()
        .cellSizeForBounds(NSRect::new(
            NSPoint::ZERO,
            NSSize::new(field.frame().size.width, 10000.0),
        ))
        .height;
    assert!(field.frame().size.height >= expected, "text must fit: {text}");
}

fn form_sheets_wrap_and_grow(mtm: objc2_foundation::MainThreadMarker, target: &super::AppDelegate) {
    use objc2::MainThreadOnly;
    use objc2_app_kit::NSView;
    use objc2_foundation::{NSPoint, NSRect, NSSize};

    let parent = super::window::MainViews::new(mtm, target);
    let form = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, NSSize::new(340.0, 24.0)));
    let sheet = super::sheet::Sheet::new(mtm);
    let title = "Remember the system with a very long name that needs more than one line?";
    let message = "Enter the peer's local name or IP address for a direct connection.";
    let error = "The address could not be saved. Check that Daisy can write its data folder, then try again without closing this dialog or losing the address you entered.";
    sheet.present(&parent.window, title, message, &form, None, "Save", move || {
        Err(error.to_owned())
    });
    let window = parent.window.attachedSheet().unwrap();
    assert_wrapped_text_fits(&window, title);
    assert_wrapped_text_fits(&window, message);
    let height = window.contentView().unwrap().frame().size.height;
    click(&window, "Save");
    assert_wrapped_text_fits(&window, error);
    assert!(
        window.contentView().unwrap().frame().size.height > height,
        "a multiline error grows the sheet"
    );
    click(&window, "Cancel");
    wait_for("the layout test sheet to close", || {
        parent.window.attachedSheet().is_none()
    });
    parent.window.close();
}

fn retention_edits_keep_deadlines_explicit(mtm: objc2_foundation::MainThreadMarker, target: &super::AppDelegate) {
    use crate::trust::Policy;
    use std::cell::Cell;
    use std::rc::Rc;

    let parent = super::window::MainViews::new(mtm, target);
    let saved = Rc::new(Cell::new(None));
    let chosen = saved.clone();
    super::trust_form::present(
        &parent.window,
        "Remember Studio until?",
        Policy::Days(4),
        move |policy| chosen.set(Some(policy)),
    );
    let sheet = parent.window.attachedSheet().unwrap();
    click(&sheet, "Save");
    wait_for("the unchanged deadline sheet to close", || {
        parent.window.attachedSheet().is_none()
    });
    assert_eq!(
        saved.get(),
        None,
        "saving an untouched fixed deadline does not renew or change it"
    );
    let chosen = saved.clone();
    super::trust_form::present(
        &parent.window,
        "Remember Studio until?",
        Policy::Days(4),
        move |policy| chosen.set(Some(policy)),
    );
    let sheet = parent.window.attachedSheet().unwrap();
    click(&sheet, "Until unused for");
    click(&sheet, "Save");
    wait_for("the chosen idle policy sheet to close", || {
        parent.window.attachedSheet().is_none()
    });
    assert_eq!(
        saved.get(),
        Some(Policy::Idle(96)),
        "explicitly choosing idle trust changes even an equal-length deadline"
    );
    parent.window.close();
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

/// Check Now must show that it is working and what it found, and turning
/// off automatic checks must disable the policy menu.
fn update_controls_report_and_follow_the_switch(mtm: objc2_foundation::MainThreadMarker, target: &super::AppDelegate) {
    use objc2::{DefinedClass, sel};
    use objc2_app_kit::NSApplication;

    target
        .ivars()
        .advanced
        .set(super::window::AdvancedViews::new(mtm, target))
        .ok()
        .expect("the test sets the Advanced sheet once");
    target.refresh_update_policy();
    let views = target.ivars().advanced.get().unwrap();
    let app = NSApplication::sharedApplication(mtm);
    assert!(
        views.update_policy.isEnabled(),
        "the menu is enabled while checks are automatic"
    );

    // SAFETY: Daisy implements these actions with an optional sender.
    assert!(unsafe { app.sendAction_to_from(sel!(toggleCheckUpdates:), Some(target), None) });
    assert!(
        !views.update_policy.isEnabled(),
        "turning automatic checks off disables the menu"
    );
    // SAFETY: as above.
    assert!(unsafe { app.sendAction_to_from(sel!(toggleCheckUpdates:), Some(target), None) });
    assert!(views.update_policy.isEnabled(), "turning them back on enables it");

    // SAFETY: as above.
    target.ivars().update_checking.set(true);
    target.refresh_update_status();
    let status = || views.update_status.stringValue().to_string();
    assert_eq!(status(), "Checking", "Check Now shows that it started");
    assert!(!views.check_now.isEnabled(), "Check Now waits for the running check");
    target.ivars().checking_ticks.set(4 * 2);
    target.refresh_update_status();
    assert_eq!(status(), "Checking..", "the dots grow while the check runs");

    let at = crate::trust::now() - 5 * 60;
    let home = tempfile::tempdir().unwrap();
    *target.ivars().update_home.borrow_mut() = home.path().to_owned();
    target.handle_update_event(sparkle_updater::UpdateEvent::DidNotFindUpdate(Default::default()));
    target.handle_update_event(sparkle_updater::UpdateEvent::DidFinishUpdateCycle(
        sparkle_updater::events::UpdateCycleInfo {
            update_check: "userInitiated".to_owned(),
            error: None,
        },
    ));
    target.ivars().update_last.borrow_mut().as_mut().unwrap().at = at;
    target.refresh_update_status();
    assert!(
        status().starts_with("Daisy is up to date. Checked ") && !status().contains("just now"),
        "the result says how long ago the check ran, got {:?}",
        status()
    );
    target.handle_update_event(sparkle_updater::UpdateEvent::DidAbortWithError(
        sparkle_updater::events::ErrorPayload {
            message: "download failed".to_owned(),
            code: 4005,
            domain: "SUSparkleErrorDomain".to_owned(),
            no_update: None,
            failure_reason: None,
            recovery_suggestion: None,
            underlying: Vec::new(),
        },
    ));
    assert!(
        status().starts_with("Could not check for updates. Last checked "),
        "a failed check keeps the last good time, got {:?}",
        status()
    );
    assert!(views.check_now.isEnabled(), "Check Now is available again");
}
