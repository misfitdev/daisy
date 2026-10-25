//! Native arrangement lifecycle checks and optional rendered animation frames.

use super::*;

/// Exercise the native view without input capture or a network session. Optional
/// frame captures render the actual AppKit view, not a separate web mockup.
pub(super) fn run(mtm: MainThreadMarker, target: &AnyObject) {
    let views = super::super::window::MainViews::new(mtm, target);
    let view = &views.arrange;
    let display = Rect {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };
    let shown = |byte: u8, name: &str, offset, active| Shown {
        key: PublicKey::from_bytes(&[byte; 32]).unwrap(),
        name: name.to_owned(),
        displays: vec![display],
        offset,
        me: byte == 1,
        in_control: Some(active),
        locked: false,
        hint: if active {
            "Has control".to_owned()
        } else {
            "Drag to rearrange".to_owned()
        },
    };
    let mut scene = vec![
        shown(1, "Local system", (0.0, 0.0), true),
        shown(2, "Studio", (1512.0, 0.0), false),
    ];
    let mut laptop = scene[0].clone();
    laptop.displays.push(Rect {
        width: 2560.0,
        height: 1440.0,
        x: 1512.0,
        y: 0.0,
    });
    assert_eq!(laptop.main(), Some(display), "identity stays on the device display");
    assert_eq!(glyph_center(display), (756.0, 491.0));
    laptop.displays.remove(0);
    assert_eq!(
        laptop.main(),
        laptop.placed().next(),
        "an external display takes over when alone"
    );
    view.show(scene.clone());
    assert!(
        view.ivars().timer.borrow().is_none(),
        "hidden views never start an animation timer"
    );
    drag_and_keyboard(view, &scene);
    let mut failed = Vec::new();
    for (name, probe) in [
        (
            "click during motion",
            click_without_drag_during_motion as fn(&ArrangeView, &[Shown]),
        ),
        (
            "selection fallback",
            selection_follows_departing_peers as fn(&ArrangeView, &[Shown]),
        ),
    ] {
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| probe(view, &scene))).is_err() {
            failed.push(name);
        }
        view.ivars().drag.set(None);
        view.take_placed();
        view.show(scene.clone());
    }
    assert!(failed.is_empty(), "native arrangement regressions: {failed:?}");

    view.ivars().keyboard.set(true);
    scene[1].offset.1 = 200.0;
    view.show(scene.clone());
    assert_eq!(
        view.ivars().motion.borrow().bodies[&scene[1].key].y.value,
        200.0,
        "keyboard placement is immediate"
    );
    assert!(
        view.take_placed().is_none(),
        "visual updates do not issue placement commands"
    );
    view.ivars().motion.borrow_mut().hold(scene[1].key, (9000.0, 0.0));
    view.show(scene.clone());
    assert_eq!(
        view.ivars().motion.borrow().bodies[&scene[1].key].x.value,
        scene[1].offset.0,
        "an unchanged authoritative response corrects a rejected drag pose"
    );
    view.ivars().keyboard.set(false);
    // A timer already running when the window hides must invalidate, even
    // if it was paused in a tracking run-loop mode at the moment of hiding.
    let timer = unsafe {
        NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
            1.0 / 60.0,
            view,
            sel!(animateArrangement:),
            None,
            true,
        )
    };
    *view.ivars().timer.borrow_mut() = Some(timer.clone());
    view.animate_arrangement(sel!(animateArrangement:), &timer);
    assert!(!timer.isValid() && view.ivars().timer.borrow().is_none());

    if let Some(directory) = std::env::var_os("DAISY_ARRANGEMENT_FRAMES") {
        let directory = std::path::PathBuf::from(directory);
        std::fs::create_dir_all(&directory).unwrap();
        views.show_status("", "");
        views.start.setHidden(true);
        views.stop.setHidden(false);
        views.show_peers(
            &["Studio", "Laptop"].map(|name| super::super::window::PeerRow {
                name: name.to_owned(),
                detail: "Connected".to_owned(),
                fingerprint: String::new(),
                trust: "Until unused for 4 days".to_owned(),
            }),
            target,
        );
        scene[1].offset.1 = 0.0;
        view.show(scene.clone());
        let transition = |scene: Vec<Shown>| {
            let mut motion = std::mem::take(&mut *view.ivars().motion.borrow_mut());
            view.show(scene.clone());
            let targets: Vec<_> = scene
                .iter()
                .map(|shown| Target {
                    key: shown.key,
                    offset: shown.offset,
                    active: shown.in_control == Some(true),
                    connected: shown.in_control.is_some(),
                })
                .collect();
            motion.sync(&targets, true);
            let size = view.bounds().size;
            let fit = Fit::of(&scene, (size.width, size.height));
            motion.frame(fit.scale, fit.origin, true);
            *view.ivars().motion.borrow_mut() = motion;
        };
        for frame in 0..120 {
            match frame {
                0 => {
                    scene.push(shown(3, "Laptop", (0.0, 982.0), false));
                    transition(scene.clone());
                }
                36 => {
                    scene[0].in_control = Some(false);
                    scene[1].in_control = Some(true);
                    transition(scene.clone());
                }
                72 => {
                    scene[2].offset = (-1512.0, 0.0);
                    transition(scene.clone());
                }
                _ => {}
            }
            view.ivars().motion.borrow_mut().step(1.0 / 30.0);
            view.redraw();
            let content = views.window.contentView().unwrap();
            // SAFETY: the retained window owns its frame for the entire capture.
            let frame_view = unsafe { content.superview() }.unwrap_or(content);
            super::super::screenshot::write_png(&frame_view, &directory.join(format!("frame-{frame:03}.png"))).unwrap();
        }
        view.ivars().motion.borrow_mut().finish();
    }
    if let Some(path) = std::env::var_os("DAISY_ARRANGEMENT_EIGHT") {
        views.show_status("", "");
        views.start.setHidden(true);
        views.stop.setHidden(false);
        let scene: Vec<_> = (1..=8)
            .map(|n| {
                shown(
                    n,
                    &format!("System {n}"),
                    (((n - 1) % 4) as f64 * 1512.0, ((n - 1) / 4) as f64 * 982.0),
                    n == 1,
                )
            })
            .collect();
        let rows: Vec<_> = (2..=8)
            .map(|n| super::super::window::PeerRow {
                name: format!("System {n}"),
                detail: "Connected · Renews while connected".into(),
                fingerprint: String::new(),
                trust: "Until unused for 4 days".into(),
            })
            .collect();
        views.show_peers(&rows, target);
        view.show(scene);
        view.ivars().motion.borrow_mut().finish();
        view.redraw();
        let content = views.window.contentView().unwrap();
        let frame_view = unsafe { content.superview() }.unwrap_or(content);
        super::super::screenshot::write_png(&frame_view, &std::path::PathBuf::from(path)).unwrap();
    }
    views.window.close();
}

/// Deliver AppKit event objects directly to this hidden test view. These are
/// never posted as system input and cannot move the desktop pointer.
fn drag_and_keyboard(view: &ArrangeView, scene: &[Shown]) {
    use objc2_app_kit::{NSEventModifierFlags, NSEventType};
    let fit = view.fit(scene);
    let rect = fit.to_view(scene[1].main().unwrap());
    let start = NSPoint::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let event = |kind, point| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, view.convertPoint_toView(point, None), NSEventModifierFlags::empty(),
        0.1, view.window().unwrap().windowNumber(), None, 0, 1, 1.0,
    ).unwrap()
    };
    view.mouse_down(sel!(mouseDown:), &event(NSEventType::LeftMouseDown, start));
    assert!(view.ivars().drag.get().is_some());
    let moved = NSPoint::new(start.x, start.y + 10.0);
    view.mouse_dragged(sel!(mouseDragged:), &event(NSEventType::LeftMouseDragged, moved));
    assert_eq!(view.fit(scene), fit, "dragging keeps its original coordinate mapping");
    view.mouse_up(sel!(mouseUp:), &event(NSEventType::LeftMouseUp, moved));
    let (key, offset) = view.take_placed().expect("the drop issues a placement");
    assert_eq!(key, scene[1].key);
    assert!((offset.0 - scene[1].offset.0).abs() < 1e-6);
    assert!((offset.1 - scene[1].offset.1 - 10.0 / fit.scale).abs() < 1e-6);
    view.show(scene.to_vec());
    view.mouse_down(sel!(mouseDown:), &event(NSEventType::LeftMouseDown, start));
    let arrow = NSString::from_str("\u{F703}");
    let key = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        NSEventType::KeyDown, NSPoint::ZERO, NSEventModifierFlags::empty(), 0.2,
        view.window().unwrap().windowNumber(), None, &arrow, &arrow, false, 124,
    ).unwrap();
    view.key_down(sel!(keyDown:), &key);
    assert!(
        view.ivars().drag.get().is_none(),
        "keyboard input cancels an interrupted drag"
    );
    assert_eq!(view.take_placed(), Some((scene[1].key, (1512.0, 0.0))));
}

fn click_without_drag_during_motion(view: &ArrangeView, original: &[Shown]) {
    use objc2_app_kit::{NSEventModifierFlags, NSEventType};
    let mut scene = original.to_vec();
    scene[1].offset.1 = 500.0;
    view.show(scene.clone());
    view.ivars()
        .motion
        .borrow_mut()
        .bodies
        .get_mut(&scene[1].key)
        .unwrap()
        .y
        .value = 250.0;
    let visual = view.visual_scene();
    let rect = view.fit(&visual).to_view(visual[1].main().unwrap());
    let point = NSPoint::new(rect.x + rect.width / 2.0, rect.y + rect.height / 2.0);
    let event = |kind| {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind, view.convertPoint_toView(point, None), NSEventModifierFlags::empty(),
        0.1, view.window().unwrap().windowNumber(), None, 0, 1, 1.0,
    ).unwrap()
    };
    view.mouse_down(sel!(mouseDown:), &event(NSEventType::LeftMouseDown));
    view.mouse_up(sel!(mouseUp:), &event(NSEventType::LeftMouseUp));
    assert!(
        view.take_placed().is_none(),
        "a click without dragging must not commit an interpolated animation pose"
    );
}

fn selection_follows_departing_peers(view: &ArrangeView, original: &[Shown]) {
    let mut scene = original.to_vec();
    let mut other = scene[1].clone();
    other.key = PublicKey::from_bytes(&[3; 32]).unwrap();
    other.name = "Other system".to_owned();
    other.offset.0 += 1512.0;
    scene.push(other.clone());
    view.show(scene.clone());
    view.select(Some(scene[1].key));
    scene.remove(1);
    view.show(scene);
    assert_eq!(view.selected(&view.ivars().scene.borrow()), Some(other.key));
    let value: Option<Retained<NSString>> = unsafe { msg_send![view, accessibilityValue] };
    assert!(
        value.is_some_and(|value| value.to_string().contains("Other system")),
        "accessibility value must name the peer that arrow keys now move"
    );
    // An unexpected accessibility-client value must also be cleared safely.
    unsafe { view.setAccessibilityValue(Some(&objc2_foundation::NSNumber::new_u8(1))) };
    view.show(vec![original[0].clone()]);
    let value: Option<Retained<NSString>> = unsafe { msg_send![view, accessibilityValue] };
    assert!(
        value.is_none(),
        "no peer selection may retain a departed peer's accessibility value"
    );
}
