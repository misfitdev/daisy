//! Native AppKit menu-bar interface for Daisy.

#[path = "app/map.rs"]
mod map;
#[path = "app/menu.rs"]
mod menu;
#[path = "app/switch.rs"]
mod switch;
#[path = "app/trust.rs"]
mod trust_form;
#[path = "app/setup.rs"]
mod walkthrough;
#[path = "app/window.rs"]
mod window;

#[cfg(test)]
#[path = "app/menu_test.rs"]
mod menu_test;

#[cfg(test)]
pub fn test_modal_menu_actions() {
    menu_test::run();
}

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy,
    NSApplicationDelegate, NSButton, NSColor, NSEventModifierFlags, NSImage, NSMenu, NSMenuItem, NSPopUpButton,
    NSSquareStatusItemLength, NSStatusBar, NSStatusItem, NSTextField, NSWindow, NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer,
    NSURL,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::control::Link;
use crate::controller::{self, AppSettings, Command, Connection, Event, Handle, SessionSettings, Status};
use crate::peers::Peer;
use crate::permissions::{self, Access};
use crate::setup::Step;

/// Matches `bundle_id` in the justfile, which writes it into Info.plist.
const BUNDLE_ID: &str = "dev.misfit.daisy";

struct AppDelegateIvars {
    controller: Handle,
    settings: RefCell<AppSettings>,
    peers: RefCell<Vec<Peer>>,
    status: RefCell<Status>,
    /// Latency and control in the running session, once reported.
    link: Cell<Option<Link>>,
    connected_at: Cell<Option<Instant>>,
    main: OnceCell<window::MainViews>,
    advanced: OnceCell<window::AdvancedViews>,
    /// This system's name, as the arrangement shows it.
    local_name: RefCell<String>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    menu_status: OnceCell<Retained<NSMenuItem>>,
    menu_start_stop: OnceCell<Retained<NSMenuItem>>,
    /// The main menu's Quit Daisy and its stand-in that closes windows; one
    /// of them carries Command-Q.
    main_quit: OnceCell<Retained<NSMenuItem>>,
    main_close_q: OnceCell<Retained<NSMenuItem>>,
    nearby: RefCell<Vec<controller::Nearby>>,
    /// The address and key of the peer last picked from Nearby; the key is
    /// used only while the address field still shows that address.
    nearby_choice: RefCell<Option<(String, String)>>,
    timer: OnceCell<Retained<NSTimer>>,
    walkthrough: OnceCell<walkthrough::SetupViews>,
    /// The walkthrough step last shown.
    setup_step: Cell<Option<Step>>,
    /// Whether the person was sent to System Settings for that step.
    setup_asked: Cell<bool>,
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
        // Back to a menu-bar-only app once the last window is gone.
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, notification: &NSNotification) {
            let closing = notification.object().map(|object| Retained::as_ptr(&object).cast::<NSWindow>());
            let others_open = self
                .windows()
                .iter()
                .any(|window| window.isVisible() && Some(Retained::as_ptr(window)) != closing);
            if !others_open {
                NSApplication::sharedApplication(self.mtm())
                    .setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            }
        }
    }

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
        if self.move_to_applications() {
            return;
        }
        self.build_main_menu();
        self.build_menu();
        self.build_window();
        let views = walkthrough::SetupViews::new(self.mtm(), self);
        views.window.setDelegate(Some(ProtocolObject::from_ref(self)));
        self.ivars().walkthrough.set(views).ok();
        self.refresh_permissions();
        if self.setup_step() != Step::Done {
            self.open_setup();
        }

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
            // Menu actions must remain reachable while a pairing or notice alert is open.
            #[unsafe(method(worksWhenModal))]
            fn works_when_modal(&self) -> bool {
                true
            }

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
            let setup_open = self.ivars().walkthrough.get().is_some_and(|views| views.window.isVisible());
            if setup_open && self.ivars().permission_poll_ticks.get().is_multiple_of(5) {
                self.refresh_setup();
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
            if let Some(views) = self.ivars().advanced.get() {
                views.address.setStringValue(&NSString::from_str(&peer.address));
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
        fn forget_peer(&self, sender: &NSButton) {
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
        fn change_trust(&self, sender: &NSButton) {
            let Some(peer) = usize::try_from(sender.tag())
                .ok()
                .and_then(|index| self.ivars().peers.borrow().get(index).cloned())
            else {
                return;
            };
            let chosen = trust_form::ask(
                self.mtm(),
                self,
                &format!("Trust {} for how long?", peer.name),
                "Save",
                Some("Cancel"),
                peer.policy,
            );
            if let Some(chosen) = chosen.filter(|chosen| *chosen != peer.policy) {
                let _ = self.ivars().controller.send(Command::SetTrust {
                    selector: peer.key.to_hex(),
                    policy: chosen,
                });
            }
        }

        // Radio buttons need a shared action to act as one group.
        #[unsafe(method(trustKind:))]
        fn trust_kind(&self, _sender: Option<&AnyObject>) {}

        #[unsafe(method(setUpPermissions:))]
        fn set_up_permissions(&self, _sender: Option<&AnyObject>) {
            self.open_setup();
        }

        #[unsafe(method(setupAllow:))]
        fn setup_allow(&self, _sender: Option<&AnyObject>) {
            match self.setup_step() {
                Step::Accessibility => self.request_accessibility(sel!(setupAllow:), None),
                Step::InputMonitoring => self.request_input_monitoring(sel!(setupAllow:), None),
                Step::Reopen | Step::Done => return,
            }
            self.ivars().setup_asked.set(true);
            self.refresh_setup();
        }

        #[unsafe(method(setupAction:))]
        fn setup_action(&self, _sender: Option<&AnyObject>) {
            match self.setup_step() {
                Step::Reopen => match crate::macos::install::relaunch() {
                    Ok(()) => NSApplication::sharedApplication(self.mtm()).terminate(None),
                    Err(error) => self.show_alert(
                        "Daisy could not reopen itself",
                        &format!("Quit Daisy from the menu bar, then open it again. ({error:#})"),
                        NSAlertStyle::Warning,
                    ),
                },
                Step::Done => {
                    if let Some(views) = self.ivars().walkthrough.get() {
                        views.window.close();
                    }
                    self.open_window();
                }
                Step::Accessibility | Step::InputMonitoring => {}
            }
        }

        #[unsafe(method(arrangePeer:))]
        fn arrange_peer(&self, sender: &AnyObject) {
            let Some(views) = self.ivars().main.get() else {
                return;
            };
            if !std::ptr::eq(sender, (&*views.arrange as &AnyObject) as *const AnyObject) {
                return;
            }
            if let Some(side) = views.arrange.side() {
                self.ivars().settings.borrow_mut().last_session.side = side;
                let _ = self.ivars().controller.send(Command::Arrange(side));
            }
        }

        #[unsafe(method(openAdvanced:))]
        fn open_advanced(&self, _sender: Option<&AnyObject>) {
            if let (Some(main), Some(advanced)) = (self.ivars().main.get(), self.ivars().advanced.get()) {
                main.window.beginSheet_completionHandler(&advanced.window, None);
            }
        }

        #[unsafe(method(closeAdvanced:))]
        fn close_advanced(&self, _sender: Option<&AnyObject>) {
            if let (Some(main), Some(advanced)) = (self.ivars().main.get(), self.ivars().advanced.get()) {
                main.window.endSheet(&advanced.window);
            }
        }

        // Command-W and Escape: a sheet ends, a window closes.
        #[unsafe(method(closeKeyWindow:))]
        fn close_key_window(&self, _sender: Option<&AnyObject>) {
            let Some(window) = NSApplication::sharedApplication(self.mtm()).keyWindow() else {
                return;
            };
            match window.sheetParent() {
                Some(parent) => parent.endSheet(&window),
                None => window.performClose(None),
            }
        }

        #[unsafe(method(closeWindows:))]
        fn close_windows(&self, _sender: Option<&AnyObject>) {
            for window in self.windows() {
                if window.isVisible() {
                    window.performClose(None);
                }
            }
        }

        #[unsafe(method(toggleCommandQ:))]
        fn toggle_command_q(&self, _sender: Option<&AnyObject>) {
            let on = {
                let mut settings = self.ivars().settings.borrow_mut();
                settings.command_q_quits = !settings.command_q_quits;
                settings.command_q_quits
            };
            let _ = self.ivars().controller.send(Command::SetCommandQQuits(on));
            self.refresh_command_q();
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
            main: OnceCell::new(),
            advanced: OnceCell::new(),
            local_name: RefCell::new(controller::computer_name()),
            status_item: OnceCell::new(),
            menu_status: OnceCell::new(),
            menu_start_stop: OnceCell::new(),
            main_quit: OnceCell::new(),
            main_close_q: OnceCell::new(),
            nearby: RefCell::new(Vec::new()),
            nearby_choice: RefCell::new(None),
            timer: OnceCell::new(),
            walkthrough: OnceCell::new(),
            setup_step: Cell::new(None),
            setup_asked: Cell::new(false),
            permission_poll_ticks: Cell::new(0),
        });
        // SAFETY: NSObject's initializer has no additional requirements.
        unsafe { msg_send![super(this), init] }
    }

    fn build_menu(&self) {
        let mtm = self.mtm();
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSSquareStatusItemLength);
        let menu = menu::new("Daisy", mtm);

        let status = self.menu_item("Stopped", None, false);
        let start_stop = self.menu_item("Start Sharing", Some(sel!(startOrStop:)), true);
        let open = self.menu_item("Open Daisy…", Some(sel!(openDaisy:)), true);
        let quit = self.menu_item("Quit Daisy", Some(sel!(quitDaisy:)), true);

        menu.addItem(&status);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&start_stop);
        menu.addItem(&open);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        if let Some(button) = status_item.button(mtm) {
            self.set_status_image(&button, false);
        }

        self.ivars().status_item.set(status_item).ok();
        self.ivars().menu_status.set(status).ok();
        self.ivars().menu_start_stop.set(start_stop).ok();
    }

    /// Offers to move a downloaded Daisy into Applications. Returns whether
    /// it moved, in which case Daisy is quitting so the copy can open.
    fn move_to_applications(&self) -> bool {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return false;
        };
        let Some(planned) = crate::macos::install::offer(&home) else {
            return false;
        };
        let app = NSApplication::sharedApplication(self.mtm());
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.activate();
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str("Move Daisy to Applications?"));
        alert.setInformativeText(&NSString::from_str(if planned.replaces {
            "This replaces the copy already there. Settings and paired peers stay."
        } else {
            "Daisy reopens from there."
        }));
        alert.addButtonWithTitle(&NSString::from_str("Move to Applications"));
        alert.addButtonWithTitle(&NSString::from_str("Not Now"));
        let moved = alert.runModal() == NSAlertFirstButtonReturn
            && match crate::macos::install::carry_out(&planned, BUNDLE_ID) {
                Ok(()) => true,
                Err(error) => {
                    self.show_alert(
                        "Daisy could not move itself",
                        &format!("Drag Daisy to the Applications folder, then open it from there. ({error:#})"),
                        NSAlertStyle::Warning,
                    );
                    false
                }
            };
        if moved {
            app.terminate(None);
        } else {
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        moved
    }

    /// The menu bar while a Daisy window is open. AppKit enables these items
    /// itself, so Close and the edit commands follow the key window.
    fn build_main_menu(&self) {
        let mtm = self.mtm();
        let item = |title: &str, action: Option<objc2::runtime::Sel>, key: &str| {
            // SAFETY: the action is implemented by Daisy or by AppKit's responder chain
            unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    action,
                    &NSString::from_str(key),
                )
            }
        };
        let submenu = |title: &str, items: &[&NSMenuItem]| {
            let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
            for entry in items {
                menu.addItem(entry);
            }
            let parent = item(title, None, "");
            parent.setSubmenu(Some(&menu));
            parent
        };

        let quit = item("Quit Daisy", Some(sel!(quitDaisy:)), "");
        let close_q = item("Close Windows", Some(sel!(closeWindows:)), "");
        close_q.setHidden(true);
        close_q.setAllowsKeyEquivalentWhenHidden(true);
        let close = item("Close", Some(sel!(closeKeyWindow:)), "w");
        let escape = item("Close", Some(sel!(closeKeyWindow:)), "\u{1b}");
        escape.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
        escape.setHidden(true);
        escape.setAllowsKeyEquivalentWhenHidden(true);
        for target in [&quit, &close_q, &close, &escape] {
            // SAFETY: Daisy implements these actions and outlives the menu
            unsafe { target.setTarget(Some(self)) };
        }
        let select_all = item("Select All", Some(sel!(selectAll:)), "a");
        let redo = item("Redo", Some(sel!(redo:)), "z");
        redo.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);

        let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
        main.addItem(&submenu("Daisy", &[&quit, &close_q]));
        main.addItem(&submenu("File", &[&close, &escape]));
        main.addItem(&submenu(
            "Edit",
            &[
                &item("Undo", Some(sel!(undo:)), "z"),
                &redo,
                &NSMenuItem::separatorItem(mtm),
                &item("Cut", Some(sel!(cut:)), "x"),
                &item("Copy", Some(sel!(copy:)), "c"),
                &item("Paste", Some(sel!(paste:)), "v"),
                &select_all,
            ],
        ));
        NSApplication::sharedApplication(mtm).setMainMenu(Some(&main));
        self.ivars().main_quit.set(quit).ok();
        self.ivars().main_close_q.set(close_q).ok();
        self.refresh_command_q();
    }

    /// Daisy's own windows, open or not.
    fn windows(&self) -> Vec<Retained<NSWindow>> {
        self.ivars()
            .main
            .get()
            .map(|views| &views.window)
            .into_iter()
            .chain(self.ivars().walkthrough.get().map(|views| &views.window))
            .cloned()
            .collect()
    }

    fn setup_step(&self) -> Step {
        let accessibility = permissions::accessibility();
        let input = permissions::input_monitoring();
        // probing only matters, and only succeeds, once both are granted
        let reads_input = permissions::ready(accessibility, input) && crate::macos::capture::reads_input();
        crate::setup::step(accessibility, input, reads_input)
    }

    fn open_setup(&self) {
        let Some(views) = self.ivars().walkthrough.get() else {
            return;
        };
        let app = NSApplication::sharedApplication(self.mtm());
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        views.window.makeKeyAndOrderFront(None);
        // macOS may refuse to activate an app launched in the background;
        // the window still comes forward
        views.window.orderFrontRegardless();
        app.activate();
        self.refresh_setup();
    }

    /// Shows the current step, bringing Daisy back to the front when one
    /// was just switched on in System Settings.
    fn refresh_setup(&self) {
        let Some(views) = self.ivars().walkthrough.get() else {
            return;
        };
        let step = self.setup_step();
        let previous = self.ivars().setup_step.replace(Some(step));
        if previous.is_some_and(|previous| previous != step) {
            self.ivars().setup_asked.set(false);
            self.refresh_permissions();
            if views.window.isVisible() {
                views.window.makeKeyAndOrderFront(None);
                views.window.orderFrontRegardless();
                NSApplication::sharedApplication(self.mtm()).activate();
            }
        }
        views.show(step, self.ivars().setup_asked.get());
    }

    fn refresh_command_q(&self) {
        let quits = self.ivars().settings.borrow().command_q_quits;
        if let Some(views) = self.ivars().main.get() {
            views.command_q.set_on(quits);
        }
        let (quit, close) = if quits { ("q", "") } else { ("", "q") };
        if let Some(item) = self.ivars().main_quit.get() {
            item.setKeyEquivalent(&NSString::from_str(quit));
        }
        if let Some(item) = self.ivars().main_close_q.get() {
            item.setKeyEquivalent(&NSString::from_str(close));
        }
    }

    fn build_window(&self) {
        let main = window::MainViews::new(self.mtm(), self);
        main.window.setDelegate(Some(ProtocolObject::from_ref(self)));
        self.ivars().main.set(main).ok();
        self.ivars()
            .advanced
            .set(window::AdvancedViews::new(self.mtm(), self))
            .ok();
        self.refresh_launch_at_login();
        self.refresh_share_clipboard();
        self.refresh_discoverable();
        self.refresh_command_q();
        self.rebuild_peers_list();
        self.render_status();
    }

    fn handle_event(&self, event: Event) {
        match event {
            Event::Nearby(nearby) => self.show_nearby(nearby),
            Event::Link { link, .. } => {
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
                self.refresh_command_q();
                *self.ivars().peers.borrow_mut() = peers;
                self.apply_settings(&settings.last_session);
                self.rebuild_peers_list();
                self.refresh_permissions();
                if first_run && self.setup_step() == Step::Done {
                    self.open_window();
                }
            }
            Event::Status(status) => self.apply_status(status),
            Event::Peers(peers) => {
                *self.ivars().peers.borrow_mut() = peers;
                self.rebuild_peers_list();
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
            Event::Paired { peer, key, policy } => {
                self.open_window();
                let chosen = trust_form::ask(
                    self.mtm(),
                    self,
                    &format!("Paired with {peer}. Trust it for how long?"),
                    "Done",
                    None,
                    policy,
                );
                if let Some(chosen) = chosen.filter(|chosen| *chosen != policy) {
                    let _ = self.ivars().controller.send(Command::SetTrust {
                        selector: key.to_hex(),
                        policy: chosen,
                    });
                }
            }
            Event::Notice { title, detail } => {
                self.show_alert(&title, &detail, NSAlertStyle::Informational);
            }
        }
    }

    fn apply_status(&self, status: Status) {
        if let Status::Connected { side, .. } = &status {
            self.ivars().settings.borrow_mut().last_session.side = *side;
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
            item.setTitle(&NSString::from_str(if is_active(&status) {
                "Stop Sharing"
            } else {
                "Start Sharing"
            }));
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
        let status = self.ivars().status.borrow();
        let copy = status_copy(&status, live);
        if let Some(views) = self.ivars().main.get() {
            let connected = match &*status {
                Status::Connected { peer, side, .. } => Some((peer.as_str(), *side, live)),
                _ => None,
            };
            let shown = map::arrangement(&self.ivars().local_name.borrow(), connected);
            let detail = if copy.connected {
                shown.link.clone()
            } else {
                copy.detail.clone()
            };
            let hint = (!shown.link_hint.is_empty()).then(|| NSString::from_str(&shown.link_hint));
            views.detail.setToolTip(hint.as_deref());
            views.title.setStringValue(&NSString::from_str(&copy.title));
            views.detail.setStringValue(&NSString::from_str(&detail));
            views.arrange.show(shown);
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
        if self.setup_step() != Step::Done {
            self.open_setup();
            return;
        }
        let Some(settings) = self.settings_from_controls() else {
            return;
        };
        let _ = self.ivars().controller.send(Command::Start {
            settings,
            allow_pairing,
            side_chosen: false,
        });
    }

    fn settings_from_controls(&self) -> Option<SessionSettings> {
        let address = self
            .ivars()
            .advanced
            .get()?
            .address
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
        let last = self.ivars().settings.borrow().last_session.clone();
        Some(SessionSettings {
            connection,
            side: last.side,
            trust: last.trust,
        })
    }

    fn apply_settings(&self, settings: &SessionSettings) {
        let address = match &settings.connection {
            Connection::Connect { address, peer } => {
                *self.ivars().nearby_choice.borrow_mut() = peer.clone().map(|key| (address.clone(), key));
                address.as_str()
            }
            Connection::Automatic => "",
        };
        if let Some(views) = self.ivars().advanced.get() {
            views.address.setStringValue(&NSString::from_str(address));
        }
    }

    fn refresh_permissions(&self) {
        let accessibility = permissions::accessibility();
        let input = permissions::input_monitoring();
        if let Some(views) = self.ivars().main.get() {
            views.show_permissions([accessibility == Access::Granted, input == Access::Granted]);
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
        if let Some(views) = self.ivars().main.get() {
            views.start.setEnabled(can_start);
            views.start.setHidden(active);
            views
                .start
                .setKeyEquivalent(&NSString::from_str(if has_peers { "\r" } else { "" }));
            self.style_action_button(&views.start, has_peers);
            views.pair.setEnabled(can_start);
            views
                .pair
                .setKeyEquivalent(&NSString::from_str(if has_peers { "" } else { "\r" }));
            self.style_action_button(&views.pair, !has_peers);
            views.stop.setEnabled(active);
            views.stop.setHidden(!active);
        }
        if let Some(item) = self.ivars().menu_start_stop.get() {
            item.setEnabled(active || can_start);
        }
    }

    fn rebuild_peers_list(&self) {
        let Some(views) = self.ivars().main.get() else {
            return;
        };
        let rows: Vec<window::PeerRow> = self
            .ivars()
            .peers
            .borrow()
            .iter()
            .map(|peer| window::PeerRow {
                name: peer.name.clone(),
                fingerprint: peer.key.fingerprint(),
                trust: peer.policy.label(),
            })
            .collect();
        views.show_peers(&rows, self);
    }

    fn refresh_discoverable(&self) {
        if let Some(views) = self.ivars().advanced.get() {
            views.discoverable.set_on(self.ivars().settings.borrow().discoverable);
        }
    }

    fn show_nearby(&self, nearby: Vec<controller::Nearby>) {
        if let Some(popup) = self.ivars().advanced.get().map(|views| &views.nearby) {
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
        if let Some(views) = self.ivars().main.get() {
            views.clipboard.set_on(self.ivars().settings.borrow().share_clipboard);
        }
    }

    fn refresh_launch_at_login(&self) {
        let Some(views) = self.ivars().main.get() else {
            return;
        };
        // SAFETY: a plain status query
        let status = unsafe { SMAppService::mainAppService().status() };
        // awaiting approval in System Settings counts as on
        views.login.set_on(matches!(
            status,
            SMAppServiceStatus::Enabled | SMAppServiceStatus::RequiresApproval
        ));
    }

    fn open_window(&self) {
        let Some(window) = self.ivars().main.get().map(|views| &views.window) else {
            return;
        };
        // paired peers may have changed from the command line
        let _ = self.ivars().controller.send(Command::Refresh);
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
    let controller = controller::spawn(home, name.clone())?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = AppDelegate::new(mtm, controller);
    *delegate.ivars().local_name.borrow_mut() = name;
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
        Status::Idle => plain("Stopped".to_owned(), "Start sharing, or pair a new peer.".to_owned()),
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
        Status::Connected { peer, .. } => {
            let title = format!("Connected to {peer}");
            let menu = match live.and_then(|(link, _)| link.latency_ms) {
                Some(ms) => format!("{title} · {ms} ms"),
                None => title.clone(),
            };
            StatusCopy {
                title,
                detail: String::new(),
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
    use crate::input::Side;

    fn connected() -> Status {
        Status::Connected {
            peer: "Studio".to_owned(),
            key: Identity::generate().unwrap().public_key(),
            side: Side::Left,
        }
    }

    #[test]
    fn the_menu_shows_latency_once_measured() {
        let link = Link {
            latency_ms: Some(4),
            in_control: false,
            ..Link::default()
        };
        let copy = status_copy(&connected(), Some((link, Duration::from_secs(65 * 60))));
        assert!(copy.connected);
        assert_eq!(copy.title, "Connected to Studio");
        assert_eq!(copy.menu, "Connected to Studio · 4 ms");

        let unmeasured = Link {
            latency_ms: None,
            in_control: true,
            ..Link::default()
        };
        let copy = status_copy(&connected(), Some((unmeasured, Duration::from_secs(30))));
        assert_eq!(copy.menu, "Connected to Studio");
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
