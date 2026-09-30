//! Native AppKit menu-bar interface for Daisy.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSControlStateValueMixed,
    NSControlStateValueOff, NSControlStateValueOn, NSFont, NSImage, NSImageView, NSMenu, NSMenuItem, NSPopUpButton,
    NSSquareStatusItemLength, NSStatusBar, NSStatusItem, NSTextField, NSView, NSWindow, NSWindowDelegate,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
    NSURL,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::control::Link;
use crate::controller::{self, AppSettings, Command, Connection, Event, Handle, SessionSettings, Status};
use crate::input::Side;
use crate::peers::Peer;
use crate::permissions::{self, Access};
use crate::trust::Policy;

/// Matches `bundle_id` in the justfile, which writes it into Info.plist.
const BUNDLE_ID: &str = "dev.misfit.daisy";
const WINDOW_WIDTH: f64 = 580.0;
const WINDOW_HEIGHT: f64 = 720.0;

struct AppDelegateIvars {
    controller: Handle,
    settings: RefCell<AppSettings>,
    peers: RefCell<Vec<Peer>>,
    status: RefCell<Status>,
    /// Latency and control in the running session, once reported.
    link: Cell<Option<Link>>,
    connected_at: Cell<Option<Instant>>,
    window: OnceCell<Retained<NSWindow>>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    menu_status: OnceCell<Retained<NSMenuItem>>,
    menu_start_stop: OnceCell<Retained<NSMenuItem>>,
    menu_launch_login: OnceCell<Retained<NSMenuItem>>,
    menu_clipboard: OnceCell<Retained<NSMenuItem>>,
    peers_menu: OnceCell<Retained<NSMenu>>,
    status_title: OnceCell<Retained<NSTextField>>,
    status_detail: OnceCell<Retained<NSTextField>>,
    connection_group: OnceCell<Retained<NSBox>>,
    address_label: OnceCell<Retained<NSTextField>>,
    address_field: OnceCell<Retained<NSTextField>>,
    nearby_popup: OnceCell<Retained<NSPopUpButton>>,
    nearby: RefCell<Vec<controller::Nearby>>,
    /// The address and key of the peer last picked from Nearby; the key is
    /// used only while the address field still shows that address.
    nearby_choice: RefCell<Option<(String, String)>>,
    menu_discoverable: OnceCell<Retained<NSMenuItem>>,
    side_label: OnceCell<Retained<NSTextField>>,
    side_popup: OnceCell<Retained<NSPopUpButton>>,
    trust_label: OnceCell<Retained<NSTextField>>,
    trust_popup: OnceCell<Retained<NSPopUpButton>>,
    permissions_heading: OnceCell<Retained<NSTextField>>,
    permissions_group: OnceCell<Retained<NSBox>>,
    accessibility_status: OnceCell<Retained<NSTextField>>,
    input_status: OnceCell<Retained<NSTextField>>,
    accessibility_button: OnceCell<Retained<NSButton>>,
    input_button: OnceCell<Retained<NSButton>>,
    start_button: OnceCell<Retained<NSButton>>,
    pair_button: OnceCell<Retained<NSButton>>,
    stop_button: OnceCell<Retained<NSButton>>,
    timer: OnceCell<Retained<NSTimer>>,
    permission_poll_ticks: Cell<u8>,
}

define_class!(
    // SAFETY: NSObject has no additional subclassing requirements and the
    // delegate is used only from AppKit's main thread.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSWindowDelegate for AppDelegate {
        // Back to a menu-bar-only app once the window is gone.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            NSApplication::sharedApplication(self.mtm()).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
    }

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
        self.build_menu();
        self.build_window();
        self.refresh_permissions();

            let timer = unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.1,
                    self,
                    sel!(pollController:),
                    None,
                    true,
                )
            };
            self.ivars().timer.set(timer).ok();
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            let _ = self.ivars().controller.send(Command::Shutdown);
        }
    }

    impl AppDelegate {
        #[unsafe(method(pollController:))]
        fn poll_controller(&self, _timer: &NSTimer) {
            loop {
                match self.ivars().controller.try_recv() {
                    Ok(Some(event)) => self.handle_event(event),
                    Ok(None) => break,
                    Err(_) => {
                        self.apply_status(Status::Problem {
                            summary: "Daisy's background controller stopped".to_owned(),
                            recovery: "Quit and reopen Daisy.".to_owned(),
                        });
                        break;
                    }
                }
            }
            let ticks = self.ivars().permission_poll_ticks.get() + 1;
            if ticks >= 20 {
                self.ivars().permission_poll_ticks.set(0);
                self.refresh_permissions();
            } else {
                self.ivars().permission_poll_ticks.set(ticks);
            }
        }

        #[unsafe(method(openDaisy:))]
        fn open_daisy(&self, _sender: Option<&AnyObject>) {
            self.open_window();
        }

        #[unsafe(method(startOrStop:))]
        fn start_or_stop(&self, _sender: Option<&AnyObject>) {
            if is_active(&self.ivars().status.borrow()) {
                let _ = self.ivars().controller.send(Command::Stop);
            } else {
                self.start(false);
            }
        }

        #[unsafe(method(startSession:))]
        fn start_session(&self, _sender: Option<&AnyObject>) {
            self.start(false);
        }

        #[unsafe(method(pairSession:))]
        fn pair_session(&self, _sender: Option<&AnyObject>) {
            self.start(true);
        }

        #[unsafe(method(stopSession:))]
        fn stop_session(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().controller.send(Command::Stop);
        }

        #[unsafe(method(resetPermissions:))]
        fn reset_permissions(&self, _sender: Option<&AnyObject>) {
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&NSString::from_str("Reset Daisy's permissions?"));
            alert.setInformativeText(&NSString::from_str(
                "Use this when System Settings shows Daisy switched on but Daisy still asks for access. \
                 macOS ties each permission to the exact copy of the app, so an entry left by an older or \
                 differently signed copy does not apply. Resetting removes Daisy's entries; Daisy then quits, \
                 and asks again when you reopen it.",
            ));
            alert.addButtonWithTitle(&NSString::from_str("Reset and Quit"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            if alert.runModal() != NSAlertFirstButtonReturn {
                return;
            }
            match permissions::reset(BUNDLE_ID) {
                Ok(()) => NSApplication::sharedApplication(self.mtm()).terminate(None),
                Err(error) => self.show_alert(
                    "Permissions could not be reset",
                    &format!(
                        "Remove Daisy from Accessibility and Input Monitoring in System Settings → \
                         Privacy & Security, then reopen Daisy. ({error})"
                    ),
                    NSAlertStyle::Warning,
                ),
            }
        }

        #[unsafe(method(requestAccessibility:))]
        fn request_accessibility(&self, _sender: Option<&AnyObject>) {
            let before = permissions::accessibility();
            let after = permissions::request_accessibility();
            open_settings(permissions::settings_after_request(before, after, permissions::ACCESSIBILITY_SETTINGS));
            self.refresh_permissions();
        }

        #[unsafe(method(requestInputMonitoring:))]
        fn request_input_monitoring(&self, _sender: Option<&AnyObject>) {
            let before = permissions::input_monitoring();
            let after = permissions::request_input_monitoring();
            open_settings(permissions::settings_after_request(
                before,
                after,
                permissions::INPUT_MONITORING_SETTINGS,
            ));
            self.refresh_permissions();
        }

        #[unsafe(method(pickNearby:))]
        fn pick_nearby(&self, sender: &NSPopUpButton) {
            // item 0 is the pull-down's title
            let index = sender.indexOfSelectedItem() - 1;
            let Some(peer) = usize::try_from(index)
                .ok()
                .and_then(|index| self.ivars().nearby.borrow().get(index).cloned())
            else {
                return;
            };
            if let Some(field) = self.ivars().address_field.get() {
                field.setStringValue(&NSString::from_str(&peer.address));
            }
            *self.ivars().nearby_choice.borrow_mut() = peer.key.map(|key| (peer.address.clone(), key.to_hex()));
        }

        #[unsafe(method(toggleDiscoverable:))]
        fn toggle_discoverable(&self, _sender: Option<&AnyObject>) {
            let on = {
                let mut settings = self.ivars().settings.borrow_mut();
                settings.discoverable = !settings.discoverable;
                settings.discoverable
            };
            let _ = self.ivars().controller.send(Command::SetDiscoverable(on));
            self.refresh_discoverable();
        }

        #[unsafe(method(toggleShareClipboard:))]
        fn toggle_share_clipboard(&self, _sender: Option<&AnyObject>) {
            let on = {
                let mut settings = self.ivars().settings.borrow_mut();
                settings.share_clipboard = !settings.share_clipboard;
                settings.share_clipboard
            };
            let _ = self.ivars().controller.send(Command::SetClipboard(on));
            self.refresh_share_clipboard();
        }

        #[unsafe(method(toggleLaunchAtLogin:))]
        fn toggle_launch_at_login(&self, _sender: Option<&AnyObject>) {
            let service = unsafe { SMAppService::mainAppService() };
        let result = if unsafe { service.status() } == SMAppServiceStatus::Enabled {
                unsafe { service.unregisterAndReturnError() }
            } else {
                unsafe { service.registerAndReturnError() }
            };
            if let Err(error) = result {
                let description = error.localizedDescription().to_string();
                self.show_alert(
                    "Launch at login could not be changed",
                    &format!("Open System Settings → General → Login Items and try again. {description}"),
                    NSAlertStyle::Informational,
                );
            }
            self.refresh_launch_at_login();
        }

        #[unsafe(method(forgetPeer:))]
        fn forget_peer(&self, sender: &NSMenuItem) {
            let index = sender.tag();
            let Some(peer) = usize::try_from(index)
                .ok()
                .and_then(|index| self.ivars().peers.borrow().get(index).cloned())
            else {
                return;
            };
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&NSString::from_str(&format!("Forget {}?", peer.name)));
            alert.setInformativeText(&NSString::from_str(
                "Any active session ends immediately. The two systems must pair again before reconnecting.",
            ));
            alert.setAlertStyle(NSAlertStyle::Informational);
            alert.addButtonWithTitle(&NSString::from_str("Forget Peer"));
            alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            if alert.runModal() == NSAlertFirstButtonReturn {
                let _ = self.ivars().controller.send(Command::Forget {
                    selector: peer.key.to_hex(),
                });
            }
        }

        #[unsafe(method(changeTrust:))]
        fn change_trust(&self, sender: &NSMenuItem) {
            let tag = sender.tag();
            if tag < 0 {
                return;
            }
            let index = usize::try_from(tag / 10).ok();
            let policy = match tag % 10 {
                0 => Some(Policy::Idle),
                1 => Some(Policy::Once),
                2 => Some(Policy::Days(30)),
                3 => Some(Policy::Forever),
                _ => None,
            };
            let Some((peer, policy)) = index
                .and_then(|index| self.ivars().peers.borrow().get(index).cloned())
                .zip(policy)
            else {
                return;
            };
            let _ = self.ivars().controller.send(Command::SetTrust {
                selector: peer.key.to_hex(),
                policy,
            });
        }

        #[unsafe(method(refreshPeers:))]
        fn refresh_peers(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().controller.send(Command::Refresh);
        }

        #[unsafe(method(quitDaisy:))]
        fn quit_daisy(&self, _sender: Option<&AnyObject>) {
            let _ = self.ivars().controller.send(Command::Shutdown);
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
    }
);

impl AppDelegate {
    fn new(mtm: MainThreadMarker, controller: Handle) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars {
            controller,
            settings: RefCell::new(AppSettings::default()),
            peers: RefCell::new(Vec::new()),
            status: RefCell::new(Status::Idle),
            link: Cell::new(None),
            connected_at: Cell::new(None),
            window: OnceCell::new(),
            status_item: OnceCell::new(),
            menu_status: OnceCell::new(),
            menu_start_stop: OnceCell::new(),
            menu_launch_login: OnceCell::new(),
            menu_clipboard: OnceCell::new(),
            peers_menu: OnceCell::new(),
            status_title: OnceCell::new(),
            status_detail: OnceCell::new(),
            connection_group: OnceCell::new(),
            address_label: OnceCell::new(),
            address_field: OnceCell::new(),
            nearby_popup: OnceCell::new(),
            nearby: RefCell::new(Vec::new()),
            nearby_choice: RefCell::new(None),
            menu_discoverable: OnceCell::new(),
            side_label: OnceCell::new(),
            side_popup: OnceCell::new(),
            trust_label: OnceCell::new(),
            trust_popup: OnceCell::new(),
            permissions_heading: OnceCell::new(),
            permissions_group: OnceCell::new(),
            accessibility_status: OnceCell::new(),
            input_status: OnceCell::new(),
            accessibility_button: OnceCell::new(),
            input_button: OnceCell::new(),
            start_button: OnceCell::new(),
            pair_button: OnceCell::new(),
            stop_button: OnceCell::new(),
            timer: OnceCell::new(),
            permission_poll_ticks: Cell::new(0),
        });
        // SAFETY: NSObject's initializer has no additional requirements.
        unsafe { msg_send![super(this), init] }
    }

    fn build_menu(&self) {
        let mtm = self.mtm();
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSSquareStatusItemLength);
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Daisy"));

        let status = self.menu_item("Stopped", None, false);
        let open = self.menu_item("Open Daisy…", Some(sel!(openDaisy:)), true);
        let start_stop = self.menu_item("Start", Some(sel!(startOrStop:)), true);
        let pair = self.menu_item("Pair a New Peer…", Some(sel!(openDaisy:)), true);
        let peers_parent = self.menu_item("Paired Peers", None, true);
        let peers_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str("Paired Peers"));
        peers_parent.setSubmenu(Some(&peers_menu));
        let clipboard = self.menu_item("Share Clipboard", Some(sel!(toggleShareClipboard:)), true);
        let launch_login = self.menu_item("Open at Login", Some(sel!(toggleLaunchAtLogin:)), true);
        let reset = self.menu_item("Reset Permissions…", Some(sel!(resetPermissions:)), true);
        let discoverable = self.menu_item("Discoverable on This Network", Some(sel!(toggleDiscoverable:)), true);
        let refresh = self.menu_item("Refresh Peers", Some(sel!(refreshPeers:)), true);
        let quit = self.menu_item("Quit Daisy", Some(sel!(quitDaisy:)), true);

        menu.addItem(&status);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&open);
        menu.addItem(&start_stop);
        menu.addItem(&pair);
        menu.addItem(&peers_parent);
        menu.addItem(&refresh);
        menu.addItem(&clipboard);
        menu.addItem(&discoverable);
        menu.addItem(&launch_login);
        menu.addItem(&reset);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        if let Some(button) = status_item.button(mtm) {
            self.set_status_image(&button, false);
        }

        self.ivars().status_item.set(status_item).ok();
        self.ivars().menu_status.set(status).ok();
        self.ivars().menu_start_stop.set(start_stop).ok();
        self.ivars().menu_launch_login.set(launch_login).ok();
        self.ivars().menu_discoverable.set(discoverable).ok();
        self.ivars().menu_clipboard.set(clipboard).ok();
        self.ivars().peers_menu.set(peers_menu).ok();
        self.refresh_launch_at_login();
        self.refresh_share_clipboard();
        self.rebuild_peers_menu();
    }

    fn build_window(&self) {
        let mtm = self.mtm();
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WINDOW_WIDTH, WINDOW_HEIGHT)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        window.setDelegate(Some(ProtocolObject::from_ref(self)));
        window.setTitle(&NSString::from_str("Daisy"));
        window.center();
        let content = window.contentView().expect("window has content view");

        let brand_image = NSImageView::imageViewWithImage(
            &flower_image(true, 54.0).expect("Daisy's bundled mark is valid SVG"),
            mtm,
        );
        brand_image.setAccessibilityLabel(Some(&NSString::from_str("Daisy")));
        brand_image.setFrame(frame(28.0, 636.0, 56.0, 56.0));
        content.addSubview(&brand_image);

        let heading = self.label("Daisy", 26.0, true);
        heading.setFrame(frame(100.0, 660.0, 452.0, 32.0));
        content.addSubview(&heading);

        let subheading = NSTextField::wrappingLabelWithString(
            &NSString::from_str("One keyboard and trackpad across your systems."),
            mtm,
        );
        subheading.setTextColor(Some(&NSColor::secondaryLabelColor()));
        subheading.setFrame(frame(100.0, 634.0, 452.0, 22.0));
        content.addSubview(&subheading);

        let separator = NSBox::initWithFrame(NSBox::alloc(mtm), frame(28.0, 612.0, 524.0, 1.0));
        separator.setBoxType(NSBoxType::Separator);
        content.addSubview(&separator);

        let status_title = self.label("Stopped", 17.0, true);
        status_title.setFrame(frame(28.0, 574.0, 524.0, 24.0));
        content.addSubview(&status_title);
        let status_detail = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Pair with a peer, then choose where its screen sits."),
            mtm,
        );
        status_detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        status_detail.setFrame(frame(28.0, 504.0, 524.0, 62.0));
        content.addSubview(&status_detail);

        let connection_heading = self.label("Connection", 15.0, true);
        connection_heading.setFrame(frame(28.0, 480.0, 524.0, 22.0));
        content.addSubview(&connection_heading);
        content.addSubview(&self.accent_rule(frame(28.0, 474.0, 42.0, 3.0)));

        let connection_group = self.group(frame(28.0, 196.0, 524.0, 274.0));
        content.addSubview(&connection_group);

        self.form_label(&content, "Nearby peers", 433.0);
        let automatic = self.label("Connect automatically on this network.", 13.0, false);
        automatic.setFrame(frame(180.0, 433.0, 352.0, 24.0));
        content.addSubview(&automatic);

        let address_label = self.form_label(&content, "Address (optional)", 389.0);
        let address = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        address.setPlaceholderString(Some(&NSString::from_str("Automatic, or enter an address")));
        address.setAccessibilityLabel(Some(&NSString::from_str("Peer address")));
        address.setFrame(frame(180.0, 384.0, 212.0, 28.0));
        content.addSubview(&address);
        let nearby =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame(400.0, 382.0, 132.0, 30.0), true);
        nearby.addItemWithTitle(&NSString::from_str("Nearby"));
        nearby.setAccessibilityLabel(Some(&NSString::from_str("Nearby peers")));
        unsafe {
            nearby.setTarget(Some(self));
            nearby.setAction(Some(sel!(pickNearby:)));
        }
        content.addSubview(&nearby);
        self.ivars().nearby_popup.set(nearby).ok();

        let side_label = self.form_label(&content, "Screen edge", 261.0);
        let side = self.popup(&content, 261.0, &["Right", "Left", "Above", "Below"], None);
        side.setAccessibilityLabel(Some(&NSString::from_str("Peer screen position")));

        let trust_label = self.form_label(&content, "Trust", 217.0);
        let trust = self.popup(
            &content,
            217.0,
            &["Until 4 days inactive", "This session", "30 days", "Until I forget"],
            None,
        );
        trust.setAccessibilityLabel(Some(&NSString::from_str("Trust duration")));

        let permissions_heading = self.label("Permissions", 13.0, true);
        permissions_heading.setFrame(frame(28.0, 162.0, 524.0, 20.0));
        content.addSubview(&permissions_heading);

        let permissions_group = self.group(frame(28.0, 66.0, 524.0, 86.0));
        content.addSubview(&permissions_group);

        let accessibility_status = self.label("Accessibility: checking…", 13.0, false);
        accessibility_status.setFrame(frame(48.0, 118.0, 290.0, 24.0));
        content.addSubview(&accessibility_status);
        let accessibility_button = self.button(
            "Grant Accessibility…",
            frame(352.0, 113.0, 180.0, 30.0),
            sel!(requestAccessibility:),
        );
        content.addSubview(&accessibility_button);

        let input_status = self.label("Input Monitoring: checking…", 13.0, false);
        input_status.setFrame(frame(48.0, 82.0, 290.0, 24.0));
        content.addSubview(&input_status);
        let input_button = self.button(
            "Grant Input Monitoring…",
            frame(352.0, 77.0, 180.0, 30.0),
            sel!(requestInputMonitoring:),
        );
        content.addSubview(&input_button);

        let start = self.button("Start Sharing", frame(282.0, 18.0, 130.0, 36.0), sel!(startSession:));
        start.setKeyEquivalent(&NSString::from_str("\r"));
        content.addSubview(&start);
        let pair = self.button("Pair a New Peer", frame(422.0, 18.0, 130.0, 36.0), sel!(pairSession:));
        content.addSubview(&pair);
        let stop = self.button("Stop Sharing", frame(422.0, 18.0, 130.0, 36.0), sel!(stopSession:));
        stop.setEnabled(false);
        stop.setHidden(true);
        content.addSubview(&stop);

        self.ivars().window.set(window).ok();
        self.ivars().status_title.set(status_title).ok();
        self.ivars().status_detail.set(status_detail).ok();
        self.ivars().connection_group.set(connection_group).ok();
        self.ivars().address_label.set(address_label).ok();
        self.ivars().address_field.set(address).ok();
        self.ivars().side_label.set(side_label).ok();
        self.ivars().side_popup.set(side).ok();
        self.ivars().trust_label.set(trust_label).ok();
        self.ivars().trust_popup.set(trust).ok();
        self.ivars().permissions_heading.set(permissions_heading).ok();
        self.ivars().permissions_group.set(permissions_group).ok();
        self.ivars().accessibility_status.set(accessibility_status).ok();
        self.ivars().input_status.set(input_status).ok();
        self.ivars().accessibility_button.set(accessibility_button).ok();
        self.ivars().input_button.set(input_button).ok();
        self.ivars().start_button.set(start).ok();
        self.ivars().pair_button.set(pair).ok();
        self.ivars().stop_button.set(stop).ok();
        self.update_conditional_controls();
    }

    fn handle_event(&self, event: Event) {
        match event {
            Event::Nearby(nearby) => self.show_nearby(nearby),
            Event::Link(link) => {
                self.ivars().link.set(Some(link));
                self.render_status();
            }
            Event::Ready {
                settings,
                peers,
                first_run,
            } => {
                *self.ivars().settings.borrow_mut() = settings.clone();
                self.refresh_discoverable();
                self.refresh_share_clipboard();
                *self.ivars().peers.borrow_mut() = peers;
                self.apply_settings(&settings.last_session);
                self.rebuild_peers_menu();
                self.refresh_permissions();
                if first_run {
                    self.open_window();
                }
            }
            Event::Status(status) => self.apply_status(status),
            Event::Peers(peers) => {
                *self.ivars().peers.borrow_mut() = peers;
                self.rebuild_peers_menu();
                self.update_action_buttons();
            }
            Event::ShowPairingCode { peer, code } => {
                self.show_alert(
                    &format!("Enter {code} on {peer}"),
                    "This one-time code is only for pairing. Daisy never asks you to compare codes by eye.",
                    NSAlertStyle::Informational,
                );
            }
            Event::AskPairingCode { peer, reply } => {
                let answer = self.ask_for_pairing_code(&peer);
                let _ = reply.send(answer.unwrap_or_default());
            }
            Event::TrustChanged { peer, policy } => {
                self.show_alert(
                    "Trust updated",
                    &format!("{peer} is now trusted {}.", policy.describe()),
                    NSAlertStyle::Informational,
                );
            }
            Event::Paired { peer, policy } => {
                self.show_alert(
                    "Peer paired",
                    &format!("{peer} is now trusted {}.", policy.describe()),
                    NSAlertStyle::Informational,
                );
            }
            Event::Notice { title, detail } => {
                self.show_alert(&title, &detail, NSAlertStyle::Informational);
            }
        }
    }

    fn apply_status(&self, status: Status) {
        if let Status::Connected { side, .. } = &status {
            self.ivars().settings.borrow_mut().last_session.side = *side;
            if let Some(popup) = self.ivars().side_popup.get() {
                popup.selectItemAtIndex(match side {
                    Side::Right => 0,
                    Side::Left => 1,
                    Side::Above => 2,
                    Side::Below => 3,
                });
            }
        }
        let was_connected = matches!(*self.ivars().status.borrow(), Status::Connected { .. });
        if !matches!(status, Status::Connected { .. }) {
            self.ivars().connected_at.set(None);
            self.ivars().link.set(None);
        } else if !was_connected {
            self.ivars().connected_at.set(Some(Instant::now()));
            self.ivars().link.set(None);
        }
        if let Some(item) = self.ivars().menu_start_stop.get() {
            item.setTitle(&NSString::from_str(if is_active(&status) { "Stop" } else { "Start" }));
        }
        let problem = matches!(status, Status::Problem { .. });
        *self.ivars().status.borrow_mut() = status;
        self.render_status();
        self.update_action_buttons();
        if problem {
            self.open_window();
        }
    }

    fn render_status(&self) {
        let live = self
            .ivars()
            .link
            .get()
            .zip(self.ivars().connected_at.get().map(|at| at.elapsed()));
        let copy = status_copy(&self.ivars().status.borrow(), live);
        if let Some(label) = self.ivars().status_title.get() {
            label.setStringValue(&NSString::from_str(&copy.title));
        }
        if let Some(label) = self.ivars().status_detail.get() {
            label.setStringValue(&NSString::from_str(&copy.detail));
        }
        if let Some(item) = self.ivars().menu_status.get() {
            item.setTitle(&NSString::from_str(&copy.menu));
        }
        if let Some(item) = self.ivars().status_item.get()
            && let Some(button) = item.button(self.mtm())
        {
            self.set_status_image(&button, copy.connected);
        }
    }

    fn start(&self, allow_pairing: bool) {
        if !self.permissions_granted() {
            self.show_alert(
                "Grant Daisy permission",
                "Daisy needs Accessibility and Input Monitoring on both systems. If System Settings already shows Daisy switched on, choose Reset Permissions in Daisy's menu.",
                NSAlertStyle::Informational,
            );
            self.open_window();
            return;
        }
        let Some(settings) = self.settings_from_controls() else {
            return;
        };
        let _ = self.ivars().controller.send(Command::Start {
            settings,
            allow_pairing,
        });
    }

    fn settings_from_controls(&self) -> Option<SessionSettings> {
        let address = self
            .ivars()
            .address_field
            .get()?
            .stringValue()
            .to_string()
            .trim()
            .to_owned();
        let connection = if address.is_empty() {
            Connection::Automatic
        } else {
            let peer = self
                .ivars()
                .nearby_choice
                .borrow()
                .as_ref()
                .filter(|(chosen, _)| *chosen == address)
                .map(|(_, key)| key.clone());
            Connection::Connect { address, peer }
        };
        let side = match self.ivars().side_popup.get()?.indexOfSelectedItem() {
            0 => Side::Right,
            1 => Side::Left,
            2 => Side::Above,
            _ => Side::Below,
        };
        let trust = match self.ivars().trust_popup.get()?.indexOfSelectedItem() {
            1 => Policy::Once,
            2 => Policy::Days(30),
            3 => Policy::Forever,
            _ => Policy::Idle,
        };
        Some(SessionSettings {
            connection,
            side,
            trust,
        })
    }

    fn apply_settings(&self, settings: &SessionSettings) {
        if let Connection::Connect { address, peer } = &settings.connection {
            *self.ivars().nearby_choice.borrow_mut() = peer.clone().map(|key| (address.clone(), key));
            if let Some(field) = self.ivars().address_field.get() {
                field.setStringValue(&NSString::from_str(address));
            }
        } else if let Some(field) = self.ivars().address_field.get() {
            field.setStringValue(&NSString::from_str(""));
        }
        if let Some(popup) = self.ivars().side_popup.get() {
            let side = settings.side;
            popup.selectItemAtIndex(match side {
                Side::Right => 0,
                Side::Left => 1,
                Side::Above => 2,
                Side::Below => 3,
            });
        }
        if let Some(popup) = self.ivars().trust_popup.get() {
            popup.selectItemAtIndex(match settings.trust {
                Policy::Idle => 0,
                Policy::Once => 1,
                Policy::Days(30) => 2,
                Policy::Forever => 3,
                Policy::Days(_) => 0,
            });
        }
        self.update_conditional_controls();
    }

    fn update_conditional_controls(&self) {
        if let Some(label) = self.ivars().side_label.get() {
            label.setFrame(frame(48.0, 345.0, 120.0, 24.0));
        }
        if let Some(popup) = self.ivars().side_popup.get() {
            popup.setFrame(frame(180.0, 340.0, 352.0, 30.0));
        }
        if let Some(label) = self.ivars().trust_label.get() {
            label.setFrame(frame(48.0, 301.0, 120.0, 24.0));
        }
        if let Some(popup) = self.ivars().trust_popup.get() {
            popup.setFrame(frame(180.0, 296.0, 352.0, 30.0));
        }
        let y = 301.0;
        let connection_bottom = y - 21.0;
        if let Some(group) = self.ivars().connection_group.get() {
            group.setFrame(frame(28.0, connection_bottom, 524.0, 470.0 - connection_bottom));
        }

        let permissions_y = connection_bottom - 34.0;
        let permissions_top = permissions_y - 10.0;
        if let Some(heading) = self.ivars().permissions_heading.get() {
            heading.setFrame(frame(28.0, permissions_y, 524.0, 20.0));
        }
        if let Some(group) = self.ivars().permissions_group.get() {
            group.setFrame(frame(28.0, permissions_top - 86.0, 524.0, 86.0));
        }
        if let Some(label) = self.ivars().accessibility_status.get() {
            label.setFrame(frame(48.0, permissions_top - 34.0, 290.0, 24.0));
        }
        if let Some(button) = self.ivars().accessibility_button.get() {
            button.setFrame(frame(352.0, permissions_top - 39.0, 180.0, 30.0));
        }
        if let Some(label) = self.ivars().input_status.get() {
            label.setFrame(frame(48.0, permissions_top - 70.0, 290.0, 24.0));
        }
        if let Some(button) = self.ivars().input_button.get() {
            button.setFrame(frame(352.0, permissions_top - 75.0, 180.0, 30.0));
        }
    }

    fn refresh_permissions(&self) {
        let accessibility = permissions::accessibility();
        let input = permissions::input_monitoring();
        if let Some(label) = self.ivars().accessibility_status.get() {
            label.setStringValue(&NSString::from_str(&permission_copy("Accessibility", accessibility)));
        }
        if let Some(button) = self.ivars().accessibility_button.get() {
            button.setEnabled(accessibility != Access::Granted);
            button.setHidden(accessibility == Access::Granted);
        }
        if let Some(label) = self.ivars().input_status.get() {
            label.setStringValue(&NSString::from_str(&permission_copy("Input Monitoring", input)));
        }
        if let Some(button) = self.ivars().input_button.get() {
            button.setEnabled(input != Access::Granted);
            button.setHidden(input == Access::Granted);
        }
        self.update_action_buttons();
    }

    fn permissions_granted(&self) -> bool {
        permissions::ready(permissions::accessibility(), permissions::input_monitoring())
    }

    fn update_action_buttons(&self) {
        let active = is_active(&self.ivars().status.borrow());
        let can_start = !active && self.permissions_granted();
        let has_peers = !self.ivars().peers.borrow().is_empty();
        if let Some(start) = self.ivars().start_button.get() {
            start.setEnabled(can_start);
            start.setHidden(active);
            start.setKeyEquivalent(&NSString::from_str(if has_peers { "\r" } else { "" }));
            self.style_action_button(start, has_peers);
        }
        if let Some(pair) = self.ivars().pair_button.get() {
            pair.setEnabled(can_start);
            pair.setHidden(active);
            pair.setKeyEquivalent(&NSString::from_str(if has_peers { "" } else { "\r" }));
            self.style_action_button(pair, !has_peers);
        }
        if let Some(stop) = self.ivars().stop_button.get() {
            stop.setEnabled(active);
            stop.setHidden(!active);
        }
        if let Some(item) = self.ivars().menu_start_stop.get() {
            item.setEnabled(active || can_start);
        }
    }

    fn rebuild_peers_menu(&self) {
        let Some(menu) = self.ivars().peers_menu.get() else {
            return;
        };
        menu.removeAllItems();
        let peers = self.ivars().peers.borrow();
        if peers.is_empty() {
            menu.addItem(&self.menu_item("No paired peers", None, false));
            return;
        }
        for (index, peer) in peers.iter().enumerate() {
            let parent = self.menu_item(&peer.name, None, true);
            let submenu = NSMenu::initWithTitle(NSMenu::alloc(self.mtm()), &NSString::from_str(&peer.name));
            let fingerprint = self.menu_item(&peer.key.fingerprint(), None, false);
            submenu.addItem(&fingerprint);
            submenu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            for (code, policy, title) in [
                (0, Policy::Idle, "Trust until 4 days inactive"),
                (1, Policy::Once, "Trust this session"),
                (2, Policy::Days(30), "Trust 30 days"),
                (3, Policy::Forever, "Trust until I forget"),
            ] {
                let item = self.menu_item(title, Some(sel!(changeTrust:)), true);
                item.setTag((index * 10 + code) as isize);
                item.setState(if peer.policy == policy {
                    NSControlStateValueOn
                } else {
                    NSControlStateValueOff
                });
                submenu.addItem(&item);
            }
            submenu.addItem(&NSMenuItem::separatorItem(self.mtm()));
            let forget = self.menu_item(&format!("Forget {}…", peer.name), Some(sel!(forgetPeer:)), true);
            forget.setTag(index as isize);
            submenu.addItem(&forget);
            parent.setSubmenu(Some(&submenu));
            menu.addItem(&parent);
        }
    }

    fn refresh_discoverable(&self) {
        if let Some(item) = self.ivars().menu_discoverable.get() {
            item.setState(if self.ivars().settings.borrow().discoverable {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
    }

    fn show_nearby(&self, nearby: Vec<controller::Nearby>) {
        if let Some(popup) = self.ivars().nearby_popup.get() {
            popup.removeAllItems();
            popup.addItemWithTitle(&NSString::from_str("Nearby"));
            if nearby.is_empty() {
                popup.addItemWithTitle(&NSString::from_str("No peers found"));
                if let Some(item) = popup.lastItem() {
                    item.setEnabled(false);
                }
            }
            for peer in &nearby {
                popup.addItemWithTitle(&NSString::from_str(&nearby_title(peer)));
            }
        }
        *self.ivars().nearby.borrow_mut() = nearby;
    }

    fn refresh_share_clipboard(&self) {
        let Some(item) = self.ivars().menu_clipboard.get() else {
            return;
        };
        item.setState(if self.ivars().settings.borrow().share_clipboard {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    }

    fn refresh_launch_at_login(&self) {
        let Some(item) = self.ivars().menu_launch_login.get() else {
            return;
        };
        let service = unsafe { SMAppService::mainAppService() };
        item.setState(match unsafe { service.status() } {
            SMAppServiceStatus::Enabled => NSControlStateValueOn,
            SMAppServiceStatus::RequiresApproval => NSControlStateValueMixed,
            _ => NSControlStateValueOff,
        });
    }

    fn open_window(&self) {
        let Some(window) = self.ivars().window.get() else {
            return;
        };
        // A menu-bar-only app is never brought forward by macOS and is missing
        // from the Dock and app switcher. While the window is open, Daisy
        // runs as a regular app so it comes to the front and can be switched to.
        let app = NSApplication::sharedApplication(self.mtm());
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        window.makeKeyAndOrderFront(None);
        app.activate();
    }

    fn ask_for_pairing_code(&self, peer: &str) -> Option<String> {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(&format!("Enter the code shown on {peer}")));
        alert.setInformativeText(&NSString::from_str(
            "The code has six digits. It is used directly to secure pairing.",
        ));
        alert.setAlertStyle(NSAlertStyle::Informational);
        alert.addButtonWithTitle(&NSString::from_str("Pair"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        let field = NSTextField::textFieldWithString(&NSString::from_str(""), self.mtm());
        field.setPlaceholderString(Some(&NSString::from_str("000-000")));
        field.setFrame(frame(0.0, 0.0, 240.0, 28.0));
        alert.setAccessoryView(Some(&field));
        if alert.runModal() == NSAlertFirstButtonReturn {
            Some(field.stringValue().to_string())
        } else {
            None
        }
    }

    fn show_alert(&self, title: &str, detail: &str, style: NSAlertStyle) {
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str(title));
        alert.setInformativeText(&NSString::from_str(detail));
        alert.setAlertStyle(style);
        alert.addButtonWithTitle(&NSString::from_str("OK"));
        alert.runModal();
    }

    fn set_status_image(&self, button: &NSButton, connected: bool) {
        if let Some(image) = flower_image(connected, 18.0) {
            button.setImage(Some(&image));
            button.setTitle(&NSString::from_str(""));
        } else {
            button.setImage(None);
            button.setTitle(&NSString::from_str("Daisy"));
        }
        button.setToolTip(Some(&NSString::from_str(if connected {
            "Daisy — Connected"
        } else {
            "Daisy — Disconnected"
        })));
    }

    fn menu_item(&self, title: &str, action: Option<objc2::runtime::Sel>, enabled: bool) -> Retained<NSMenuItem> {
        let item = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(self.mtm()),
                &NSString::from_str(title),
                action,
                &NSString::from_str(""),
            )
        };
        if action.is_some() {
            unsafe { item.setTarget(Some(self)) };
        }
        item.setEnabled(enabled);
        item
    }

    fn label(&self, text: &str, size: f64, bold: bool) -> Retained<NSTextField> {
        let label = NSTextField::labelWithString(&NSString::from_str(text), self.mtm());
        let font = if bold {
            NSFont::boldSystemFontOfSize(size)
        } else {
            NSFont::systemFontOfSize(size)
        };
        label.setFont(Some(&font));
        label
    }

    fn form_label(&self, content: &NSView, text: &str, y: f64) -> Retained<NSTextField> {
        let label = self.label(text, 13.0, false);
        label.setTextColor(Some(&NSColor::secondaryLabelColor()));
        label.setFrame(frame(48.0, y, 120.0, 24.0));
        content.addSubview(&label);
        label
    }

    fn group(&self, rect: NSRect) -> Retained<NSBox> {
        let group = NSBox::initWithFrame(NSBox::alloc(self.mtm()), rect);
        group.setBoxType(NSBoxType::Custom);
        group.setTransparent(false);
        group.setBorderWidth(0.0);
        group.setCornerRadius(16.0);
        group.setFillColor(&NSColor::tertiarySystemFillColor());
        group
    }

    fn accent_rule(&self, rect: NSRect) -> Retained<NSBox> {
        let accent = NSBox::initWithFrame(NSBox::alloc(self.mtm()), rect);
        accent.setBoxType(NSBoxType::Custom);
        accent.setTransparent(false);
        accent.setBorderWidth(0.0);
        accent.setCornerRadius(1.5);
        accent.setFillColor(&coral_color());
        accent
    }

    fn popup(
        &self,
        content: &NSView,
        y: f64,
        items: &[&str],
        action: Option<objc2::runtime::Sel>,
    ) -> Retained<NSPopUpButton> {
        let popup = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(self.mtm()),
            frame(180.0, y - 5.0, 352.0, 30.0),
            false,
        );
        for item in items {
            popup.addItemWithTitle(&NSString::from_str(item));
        }
        if let Some(action) = action {
            unsafe {
                popup.setTarget(Some(self));
                popup.setAction(Some(action));
            }
        }
        content.addSubview(&popup);
        popup
    }

    fn button(&self, title: &str, rect: NSRect, action: objc2::runtime::Sel) -> Retained<NSButton> {
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(self), Some(action), self.mtm())
        };
        button.setFrame(rect);
        button
    }

    fn style_action_button(&self, button: &NSButton, primary: bool) {
        if primary {
            button.setBezelColor(Some(&coral_color()));
            button.setContentTintColor(Some(&NSColor::whiteColor()));
        } else {
            button.setBezelColor(None);
            button.setContentTintColor(None);
        }
    }
}

pub fn run(home: PathBuf, name: String) -> Result<()> {
    let mtm = MainThreadMarker::new().context("Daisy's interface must start on the main thread")?;
    let controller = controller::spawn(home, name)?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm, controller);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    crate::macos::main_run_loop_starting();
    app.run();
    Ok(())
}

/// What the window and menu say about the session.
#[derive(Debug, PartialEq, Eq)]
struct StatusCopy {
    title: String,
    detail: String,
    /// The menu's status line: the title, with latency once measured.
    menu: String,
    connected: bool,
}

/// `live` is the running session's latest report and how long it has run.
fn status_copy(status: &Status, live: Option<(Link, Duration)>) -> StatusCopy {
    let plain = |title: String, detail: String| StatusCopy {
        menu: title.clone(),
        title,
        detail,
        connected: false,
    };
    match status {
        Status::Idle => plain(
            "Stopped".to_owned(),
            "Pair with a peer, then choose where its screen sits.".to_owned(),
        ),
        Status::Starting => plain("Starting…".to_owned(), "Daisy is preparing the connection.".to_owned()),
        Status::Waiting {
            port,
            pairing,
            looking_for,
        } => plain(
            match (pairing, looking_for) {
                (true, _) => "Waiting to pair".to_owned(),
                (false, Some(peer)) => format!("Looking for {peer}"),
                (false, None) => "Looking for peers".to_owned(),
            },
            format!("This system is available on port {port}."),
        ),
        Status::Connecting { address, peer } => plain(
            peer.as_ref()
                .map_or_else(|| "Connecting…".to_owned(), |peer| format!("Connecting to {peer}…")),
            format!("Reaching {address}."),
        ),
        Status::Reconnecting { address, peer, wait } => plain(
            peer.as_ref()
                .map_or_else(|| "Reconnecting…".to_owned(), |peer| format!("Reconnecting to {peer}…")),
            format!(
                "The connection to {} was lost. Trying again in {} s.",
                peer.as_deref().unwrap_or(address),
                wait.as_secs().max(1)
            ),
        ),
        Status::Connected { peer, side, .. } => {
            let title = format!("Connected to {peer}");
            let mut facts = Vec::new();
            let mut menu = title.clone();
            if let Some((link, connected_for)) = live {
                facts.push(if link.in_control {
                    "This system has control.".to_owned()
                } else {
                    format!("{peer} has control.")
                });
                if let Some(ms) = link.latency_ms {
                    facts.push(format!("Round trip {ms} ms."));
                    menu = format!("{title} · {ms} ms");
                }
                facts.push(format!("Connected for {}.", span(connected_for)));
            }
            facts.push(format!(
                "Move through the {side} edge to use {peer}. Use each system's own keyboard or trackpad to take control there. Press Control-Option-Command-Escape to return."
            ));
            StatusCopy {
                title,
                detail: facts.join(" "),
                menu,
                connected: true,
            }
        }
        Status::Problem { summary, recovery } => plain(summary.clone(), recovery.clone()),
    }
}

/// How long a session has run, to the minute.
fn span(elapsed: Duration) -> String {
    let minutes = elapsed.as_secs() / 60;
    match (minutes / 60, minutes % 60) {
        (0, 0) => "less than a minute".to_owned(),
        (0, minutes) => format!("{minutes} min"),
        (hours, 0) => format!("{hours} h"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    }
}

fn is_active(status: &Status) -> bool {
    matches!(
        status,
        Status::Starting
            | Status::Waiting { .. }
            | Status::Connecting { .. }
            | Status::Reconnecting { .. }
            | Status::Connected { .. }
    )
}

fn open_settings(pane: Option<&str>) {
    let Some(pane) = pane else { return };
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(pane)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

fn nearby_title(peer: &controller::Nearby) -> String {
    match &peer.name {
        Some(name) => name.clone(),
        None => format!("Peer open to pairing at {}", peer.address),
    }
}

fn permission_copy(name: &str, access: Access) -> String {
    match access {
        Access::Granted => format!("{name}: Granted"),
        Access::Denied => format!("{name}: Needs access"),
        Access::Undetermined => format!("{name}: Not requested"),
    }
}

fn coral_color() -> Retained<NSColor> {
    NSColor::colorWithRed_green_blue_alpha(1.0, 107.0 / 255.0, 94.0 / 255.0, 1.0)
}

fn flower_image(connected: bool, size: f64) -> Option<Retained<NSImage>> {
    let center = if connected { "#FFD447" } else { "#A3A7AA" };
    let mut petals = String::new();
    for angle in (0..360).step_by(45) {
        petals.push_str(&format!(
            "<ellipse cx='12' cy='5' rx='2.1' ry='4' transform='rotate({angle} 12 12)'/>"
        ));
    }
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='24' height='24' viewBox='0 0 24 24'><g fill='#737D84'>{petals}</g><circle cx='12' cy='12' r='2.7' fill='{center}'/></svg>"
    );
    let data = unsafe { NSData::dataWithBytes_length(svg.as_ptr().cast::<c_void>(), svg.len()) };
    let image = NSImage::initWithData(NSImage::alloc(), &data)?;
    image.setSize(NSSize::new(size, size));
    image.setTemplate(false);
    Some(image)
}

fn frame(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn connected() -> Status {
        Status::Connected {
            peer: "Studio".to_owned(),
            key: Identity::generate().unwrap().public_key(),
            side: Side::Left,
        }
    }

    #[test]
    fn connected_copy_keeps_recovery_chord_visible() {
        let copy = status_copy(&connected(), None);
        assert!(copy.connected);
        assert!(copy.detail.contains("Control-Option-Command-Escape"));
    }

    #[test]
    fn a_running_session_shows_control_latency_and_duration() {
        let link = Link {
            latency_ms: Some(4),
            in_control: false,
        };
        let copy = status_copy(&connected(), Some((link, Duration::from_secs(65 * 60))));
        assert_eq!(copy.title, "Connected to Studio");
        assert_eq!(copy.menu, "Connected to Studio · 4 ms");
        assert!(
            copy.detail
                .starts_with("Studio has control. Round trip 4 ms. Connected for 1 h 5 min.")
        );
        assert!(copy.detail.contains("Control-Option-Command-Escape"));

        let here = Link {
            latency_ms: None,
            in_control: true,
        };
        let copy = status_copy(&connected(), Some((here, Duration::from_secs(30))));
        assert_eq!(copy.menu, "Connected to Studio");
        assert!(
            copy.detail
                .starts_with("This system has control. Connected for less than a minute.")
        );
    }

    #[test]
    fn waiting_names_the_peer_it_is_looking_for() {
        let waiting = |pairing, looking_for: Option<&str>| {
            status_copy(
                &Status::Waiting {
                    port: crate::service::DEFAULT_PORT,
                    pairing,
                    looking_for: looking_for.map(str::to_owned),
                },
                None,
            )
            .title
        };
        assert_eq!(waiting(true, Some("Studio")), "Waiting to pair");
        assert_eq!(waiting(false, Some("Studio")), "Looking for Studio");
        assert_eq!(waiting(false, None), "Looking for peers");
    }

    #[test]
    fn connecting_and_reconnecting_name_a_known_peer() {
        let connecting = status_copy(
            &Status::Connecting {
                address: "192.168.1.20:24850".to_owned(),
                peer: Some("Studio".to_owned()),
            },
            None,
        );
        assert_eq!(connecting.title, "Connecting to Studio…");
        let reconnecting = status_copy(
            &Status::Reconnecting {
                address: "192.168.1.20:24850".to_owned(),
                peer: Some("Studio".to_owned()),
                wait: Duration::from_secs(2),
            },
            None,
        );
        assert_eq!(reconnecting.title, "Reconnecting to Studio…");
        assert!(reconnecting.detail.starts_with("The connection to Studio was lost."));
        let unknown = status_copy(
            &Status::Connecting {
                address: "192.168.1.20:24850".to_owned(),
                peer: None,
            },
            None,
        );
        assert_eq!(unknown.title, "Connecting…");
        assert_eq!(status_copy(&Status::Idle, None).title, "Stopped");
    }

    #[test]
    fn spans_read_to_the_minute() {
        assert_eq!(span(Duration::from_secs(59)), "less than a minute");
        assert_eq!(span(Duration::from_secs(12 * 60 + 30)), "12 min");
        assert_eq!(span(Duration::from_secs(2 * 3600)), "2 h");
    }
}
