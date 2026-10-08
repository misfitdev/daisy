//! Native AppKit menu-bar interface for Daisy.

#[path = "app/map.rs"]
mod map;
#[path = "app/menu.rs"]
mod menu;
#[path = "app/panel.rs"]
mod panel;
#[path = "app/screenshot.rs"]
pub mod screenshot;
#[path = "app/sheet.rs"]
mod sheet;
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

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionKey, NSAboutPanelOptionVersion, NSAccessibility, NSAlert,
    NSAlertFirstButtonReturn, NSAlertStyle, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSButton, NSColor, NSEventModifierFlags, NSImage, NSMenu, NSMenuItem, NSModalResponse, NSPopUpButton,
    NSSquareStatusItemLength, NSStatusBar, NSStatusItem, NSWindow, NSWindowDelegate, NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSData, NSDate, NSDateFormatter, NSDictionary, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL,
};
use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::control::Link;
use crate::controller::{self, AppSettings, Command, Event, Handle, SessionSettings, Status};
use crate::peers::Peer;
use crate::permissions::{self, Access};
use crate::setup::Step;

/// Matches `bundle_id` in the justfile, which writes it into Info.plist.
const BUNDLE_ID: &str = "dev.misfit.daisy";
/// How long the Connected panel stays before closing on its own.
const CONNECTED_SHOWN: std::time::Duration = std::time::Duration::from_secs(15);
/// Past these, the exchange has timed out on the network side as well.
const CHECKING_SHOWN: std::time::Duration = std::time::Duration::from_secs(20);
const CODE_SHOWN: std::time::Duration = std::time::Duration::from_secs(120);
/// How long past its countdown Add a System waits to hear that it closed.
const WAITING_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

struct AppDelegateIvars {
    controller: Handle,
    settings: RefCell<AppSettings>,
    peers: RefCell<Vec<Peer>>,
    status: RefCell<Status>,
    /// This system's key, once the controller reports it.
    me: Cell<Option<crate::identity::PublicKey>>,
    /// Each running link, as it last reported.
    links: RefCell<std::collections::BTreeMap<crate::identity::PublicKey, Link>>,
    /// Where every member's displays sit, while the group runs.
    arranged: RefCell<Option<crate::share::Layout>>,
    main: OnceCell<window::MainViews>,
    advanced: OnceCell<window::AdvancedViews>,
    /// Whether the running group is open to a new system.
    adding: Cell<bool>,
    /// This system's name, as the arrangement shows it.
    local_name: RefCell<String>,
    status_item: OnceCell<Retained<NSStatusItem>>,
    menu_start_stop: OnceCell<Retained<NSMenuItem>>,
    menu_add: OnceCell<Retained<NSMenuItem>>,
    pairing: OnceCell<panel::Panel>,
    /// Where a typed code goes, while one is asked for.
    code_reply: RefCell<Option<tokio::sync::oneshot::Sender<String>>>,
    /// When the Connected panel appeared, to close it on its own.
    connected_at: Cell<Option<std::time::Instant>>,
    /// Touch ID or password result for Always Discoverable, from AppKit's
    /// callback thread.
    owner_confirmed: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
    /// Whether a configuration profile allows Always Discoverable.
    always_allowed: Cell<bool>,
    timer: OnceCell<Retained<NSTimer>>,
    walkthrough: OnceCell<walkthrough::SetupViews>,
    /// The walkthrough step last shown.
    setup_step: Cell<Option<Step>>,
    /// Whether the person was sent to System Settings for that step.
    setup_asked: Cell<bool>,
    permission_poll_ticks: Cell<u8>,
    /// Counts toward refreshing peer expiry captions, which otherwise only
    /// change in response to a peer or link event.
    expiry_poll_ticks: Cell<u16>,
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
            let _ = self.ivars().controller.send(Command::Startup(crate::install::update::StartupState::UiReady));
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, _notification: &NSNotification) {
            if let Err(error) = self.ivars().controller.shutdown() {
                tracing::warn!(error = ?error, "Daisy's sharing tasks did not stop before exit");
            }
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
            self.tick_pairing();
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
            // 0.1s per tick; expiry captions only need minute-level freshness
            const EXPIRY_REFRESH_TICKS: u16 = 600;
            let main_open = self.ivars().main.get().is_some_and(|views| views.window.isVisible());
            if main_open {
                let expiry_ticks = self.ivars().expiry_poll_ticks.get() + 1;
                if expiry_ticks >= EXPIRY_REFRESH_TICKS {
                    self.ivars().expiry_poll_ticks.set(0);
                    self.rebuild_peers_list();
                } else {
                    self.ivars().expiry_poll_ticks.set(expiry_ticks);
                }
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
                self.start();
            }
        }

        #[unsafe(method(startSession:))]
        fn start_session(&self, _sender: Option<&AnyObject>) {
            self.start();
        }

        #[unsafe(method(addPeerByAddress:))]
        fn add_peer_by_address(&self, _sender: Option<&AnyObject>) {
            if let Some(views) = self.ivars().main.get() {
                self.ask_peer_address(&views.window);
            }
        }

        #[unsafe(method(addSystem:))]
        fn add_system(&self, _sender: Option<&AnyObject>) {
            self.add_a_system();
        }

        #[unsafe(method(pairingPrimary:))]
        fn pairing_primary(&self, _sender: Option<&AnyObject>) {
            let Some(panel) = self.ivars().pairing.get() else {
                return;
            };
            match panel.state() {
                Some(panel::State::EnterCode { peer, .. }) => {
                    let text = panel.field.stringValue().to_string();
                    if crate::pairing::PairingCode::parse(text.trim()).is_none() {
                        let problem = Some(format!("Enter the 6 digits shown on {peer}."));
                        panel.show(panel::State::EnterCode { peer, problem }, true);
                        return;
                    }
                    if let Some(reply) = self.ivars().code_reply.borrow_mut().take() {
                        let _ = reply.send(text);
                    }
                    panel.show(panel::State::Checking { peer }, false);
                }
                Some(panel::State::NoneJoined) => self.add_a_system(),
                Some(panel::State::Connected { peer }) => {
                    panel.close();
                    self.open_window();
                    if let Some(views) = self.ivars().main.get() {
                        let key = self.ivars().peers.borrow().iter().find(|p| p.name == peer).map(|p| p.key);
                        views.arrange.select(key);
                    }
                }
                _ => {}
            }
        }

        #[unsafe(method(pairingSecondary:))]
        fn pairing_secondary(&self, _sender: Option<&AnyObject>) {
            let Some(panel) = self.ivars().pairing.get() else {
                return;
            };
            match panel.state() {
                Some(panel::State::Waiting { .. }) => {
                    let _ = self.ivars().controller.send(Command::CancelAdding);
                }
                Some(panel::State::EnterCode { .. }) => {
                    // an empty code ends the exchange on both systems
                    if let Some(reply) = self.ivars().code_reply.borrow_mut().take() {
                        let _ = reply.send(String::new());
                    }
                    let _ = self.ivars().controller.send(Command::CancelAdding);
                }
                _ => {}
            }
            panel.close();
        }

        #[unsafe(method(toggleAlwaysDiscoverable:))]
        fn toggle_always_discoverable(&self, _sender: Option<&AnyObject>) {
            let on = self.ivars().settings.borrow().always_discoverable;
            if on {
                self.set_always_discoverable(false);
            } else if self.confirm_always_discoverable() {
                self.confirm_owner();
            }
            self.refresh_always_discoverable();
        }

        #[unsafe(method(showAbout:))]
        fn show_about(&self, _sender: Option<&AnyObject>) {
            let app = NSApplication::sharedApplication(self.mtm());
            app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
            app.activate();
            // SAFETY: AppKit's constant keys
            let keys = unsafe { [NSAboutPanelOptionApplicationVersion, NSAboutPanelOptionVersion] };
            let options = NSDictionary::<NSAboutPanelOptionKey, AnyObject>::from_slices(
                &keys,
                &[
                    NSString::from_str(env!("CARGO_PKG_VERSION")).as_ref(),
                    NSString::from_str(crate::COMMIT).as_ref(),
                ],
            );
            // SAFETY: every option is a string, as AppKit expects
            unsafe { app.orderFrontStandardAboutPanelWithOptions(&options) };
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


    #[unsafe(method(selectUpdatePolicy:))]
    fn select_update_policy(&self, sender: &NSPopUpButton) {
        let Some(policy) = usize::try_from(sender.indexOfSelectedItem())
            .ok()
            .and_then(|index| crate::update::ORDER.get(index).copied())
        else {
            return;
        };
        self.ivars().settings.borrow_mut().update_policy = policy;
        let _ = self.ivars().controller.send(Command::SetUpdatePolicy(policy));
        self.refresh_update_policy();
    }

    #[unsafe(method(checkForUpdatesNow:))]
    fn check_for_updates_now(&self, _sender: Option<&AnyObject>) {
        if let Some(views) = self.ivars().advanced.get() {
            views.check_now.setEnabled(false);
            views.update_status.setStringValue(&NSString::from_str("Checking…"));
        }
        let _ = self.ivars().controller.send(Command::CheckUpdatesNow);
    }

    #[unsafe(method(toggleCheckUpdates:))]
    fn toggle_check_updates(&self, _sender: Option<&AnyObject>) {
        let on = {
            let mut settings = self.ivars().settings.borrow_mut();
            settings.check_updates = !settings.check_updates;
            settings.check_updates
        };
        let _ = self.ivars().controller.send(Command::SetCheckUpdates(on));
        self.refresh_update_policy();
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
            let Some(window) = sender.window() else {
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
            let this = self.retain();
            let handler = block2::RcBlock::new(move |response: NSModalResponse| {
                if response == NSAlertFirstButtonReturn {
                    let _ = this.ivars().controller.send(Command::Forget {
                        selector: peer.key.to_hex(),
                    });
                }
            });
            alert.beginSheetModalForWindow_completionHandler(&window, Some(&handler));
        }

        #[unsafe(method(changeTrust:))]
        fn change_trust(&self, sender: &NSButton) {
            let Some(peer) = usize::try_from(sender.tag())
                .ok()
                .and_then(|index| self.ivars().peers.borrow().get(index).cloned())
            else {
                return;
            };
            let Some(window) = sender.window() else {
                return;
            };
            let this = self.retain();
            let key = peer.key;
            trust_form::present(&window, &format!("Remember {} until?", peer.name), peer.policy, move |policy| {
                let _ = this.ivars().controller.send(Command::SetTrust {
                    selector: key.to_hex(),
                    policy,
                });
            });
        }

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
        fn arrange_peer(&self, _sender: Option<&AnyObject>) {
            let placed = self.ivars().main.get().and_then(|views| views.arrange.take_placed());
            if let Some((key, offset)) = placed {
                let _ = self.ivars().controller.send(Command::Place(key, offset));
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
            me: Cell::new(None),
            links: RefCell::new(std::collections::BTreeMap::new()),
            arranged: RefCell::new(None),
            main: OnceCell::new(),
            advanced: OnceCell::new(),
            adding: Cell::new(false),
            local_name: RefCell::new(controller::computer_name()),
            status_item: OnceCell::new(),
            menu_start_stop: OnceCell::new(),
            menu_add: OnceCell::new(),
            pairing: OnceCell::new(),
            code_reply: RefCell::new(None),
            connected_at: Cell::new(None),
            owner_confirmed: std::sync::Arc::new(std::sync::Mutex::new(None)),
            always_allowed: Cell::new(true),
            timer: OnceCell::new(),
            walkthrough: OnceCell::new(),
            setup_step: Cell::new(None),
            setup_asked: Cell::new(false),
            permission_poll_ticks: Cell::new(0),
            expiry_poll_ticks: Cell::new(0),
        });
        // SAFETY: NSObject's initializer has no additional requirements.
        unsafe { msg_send![super(this), init] }
    }

    fn build_menu(&self) {
        let mtm = self.mtm();
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSSquareStatusItemLength);
        let menu = menu::new("Daisy", mtm);

        let open = self.menu_item("Open Daisy", Some(sel!(openDaisy:)), true);
        let start_stop = self.menu_item("Start Sharing", Some(sel!(startOrStop:)), true);
        let add = self.menu_item("Add a System", Some(sel!(addSystem:)), false);
        let quit = self.menu_item("Quit", Some(sel!(quitDaisy:)), true);

        menu.addItem(&open);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&start_stop);
        menu.addItem(&add);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&quit);
        status_item.setMenu(Some(&menu));

        if let Some(button) = status_item.button(mtm) {
            self.set_status_image(&button, false);
        }

        self.ivars().status_item.set(status_item).ok();
        self.ivars().menu_start_stop.set(start_stop).ok();
        self.ivars().menu_add.set(add).ok();
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

        let about = item("About Daisy", Some(sel!(showAbout:)), "");
        let quit = item("Quit Daisy", Some(sel!(quitDaisy:)), "");
        // Command-Q closes windows; quitting stops sharing, so it is only in
        // the menu-bar flower and this menu
        let close_q = item("Close Windows", Some(sel!(closeWindows:)), "q");
        close_q.setHidden(true);
        close_q.setAllowsKeyEquivalentWhenHidden(true);
        let close = item("Close", Some(sel!(closeKeyWindow:)), "w");
        let escape = item("Close", Some(sel!(closeKeyWindow:)), "\u{1b}");
        escape.setKeyEquivalentModifierMask(NSEventModifierFlags::empty());
        escape.setHidden(true);
        escape.setAllowsKeyEquivalentWhenHidden(true);
        for target in [&about, &quit, &close_q, &close, &escape] {
            // SAFETY: Daisy implements these actions and outlives the menu
            unsafe { target.setTarget(Some(self)) };
        }
        let select_all = item("Select All", Some(sel!(selectAll:)), "a");
        let redo = item("Redo", Some(sel!(redo:)), "z");
        redo.setKeyEquivalentModifierMask(NSEventModifierFlags::Command | NSEventModifierFlags::Shift);

        let main = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(""));
        main.addItem(&submenu(
            "Daisy",
            &[&about, &NSMenuItem::separatorItem(mtm), &quit, &close_q],
        ));
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

    fn build_window(&self) {
        let main = window::MainViews::new(self.mtm(), self);
        main.window.setDelegate(Some(ProtocolObject::from_ref(self)));
        self.ivars().main.set(main).ok();
        self.ivars().pairing.set(panel::Panel::new(self.mtm(), self)).ok();
        self.ivars()
            .advanced
            .set(window::AdvancedViews::new(self.mtm(), self))
            .ok();
        self.refresh_launch_at_login();
        self.refresh_share_clipboard();
        self.refresh_update_policy();
        self.rebuild_peers_list();
        self.render_status();
    }

    fn handle_event(&self, event: Event) {
        match event {
            Event::Link { key, link } => {
                self.ivars().links.borrow_mut().insert(key, link);
                self.render_status();
                self.rebuild_peers_list();
            }
            Event::Arranged(layout) => {
                *self.ivars().arranged.borrow_mut() = Some(layout);
                self.render_status();
            }
            Event::Ready {
                settings,
                peers,
                first_run,
                me,
                always_discoverable_allowed,
            } => {
                self.ivars().me.set(Some(me));
                self.ivars().always_allowed.set(always_discoverable_allowed);
                *self.ivars().settings.borrow_mut() = settings.clone();
                self.refresh_share_clipboard();
                self.refresh_update_policy();
                self.refresh_always_discoverable();
                *self.ivars().peers.borrow_mut() = peers;
                self.apply_settings(&settings.last_session);
                self.rebuild_peers_list();
                self.refresh_permissions();
                self.render_status();
                if first_run {
                    self.open_at_login();
                }
                if first_run && self.setup_step() == Step::Done {
                    self.open_window();
                }
                if settings.sharing && self.setup_step() == Step::Done {
                    self.start();
                }
                if !settings.sharing {
                    let _ = self
                        .ivars()
                        .controller
                        .send(Command::Startup(crate::install::update::StartupState::IdleReady));
                }
            }
            Event::Status(status) => self.apply_status(status),
            Event::Adding(adding) => {
                self.ivars().adding.set(adding);
                if !adding
                    && let Some(panel) = self.ivars().pairing.get()
                    && matches!(panel.state(), Some(panel::State::Waiting { .. }))
                {
                    panel.show(panel::State::NoneJoined, false);
                }
                self.render_status();
                self.update_action_buttons();
            }
            Event::Peers(peers) => {
                *self.ivars().peers.borrow_mut() = peers;
                self.rebuild_peers_list();
                self.update_action_buttons();
            }
            Event::ShowPairingCode { peer, code } => {
                if let Some(panel) = self.ivars().pairing.get() {
                    panel.show(panel::State::ShowCode { peer, code }, false);
                }
            }
            Event::AskPairingCode { peer, reply } => {
                *self.ivars().code_reply.borrow_mut() = Some(reply);
                if let Some(panel) = self.ivars().pairing.get() {
                    // only take the keyboard when this system asked to add one
                    let asked = matches!(panel.state(), Some(panel::State::Waiting { .. }))
                        || NSApplication::sharedApplication(self.mtm()).isActive();
                    panel.show(panel::State::EnterCode { peer, problem: None }, asked);
                }
            }
            Event::Paired { peer, key: _ } => {
                if let Some(panel) = self.ivars().pairing.get() {
                    panel.show(panel::State::Connected { peer }, false);
                    self.ivars().connected_at.set(Some(std::time::Instant::now()));
                }
            }
            Event::CodeMismatch => {
                if let Some(panel) = self.ivars().pairing.get()
                    && matches!(
                        panel.state(),
                        Some(panel::State::ShowCode { .. } | panel::State::Checking { .. })
                    )
                {
                    panel.show(panel::State::Mismatch, false);
                }
            }
            Event::UpdateChecked(summary) => {
                if let Some(views) = self.ivars().advanced.get() {
                    views.check_now.setEnabled(true);
                    views.update_status.setStringValue(&NSString::from_str(&format!(
                        "{summary} Checked {}.",
                        format_time(crate::trust::now())
                    )));
                }
            }
            Event::Notice { title, detail } => {
                self.show_alert(&title, &detail, NSAlertStyle::Informational);
            }
        }
    }

    fn apply_status(&self, status: Status) {
        let milestone = match &status {
            Status::Waiting { .. } | Status::Connected { .. } => {
                Some(crate::install::update::StartupState::SharingReady)
            }
            Status::Problem { .. } => Some(crate::install::update::StartupState::Failed),
            _ => None,
        };
        if let Some(milestone) = milestone {
            let _ = self.ivars().controller.send(Command::Startup(milestone));
        }
        let problem = matches!(status, Status::Problem { .. });
        match &status {
            Status::Connected { peers } => self
                .ivars()
                .links
                .borrow_mut()
                .retain(|key, _| peers.iter().any(|(_, linked)| linked == key)),
            _ => {
                self.ivars().links.borrow_mut().clear();
                *self.ivars().arranged.borrow_mut() = None;
                self.rebuild_peers_list();
            }
        }
        if !matches!(status, Status::Connected { .. }) {
            self.ivars().adding.set(false);
        }
        *self.ivars().status.borrow_mut() = status;
        self.render_status();
        self.update_action_buttons();
        if problem {
            self.open_window();
        }
    }

    fn render_status(&self) {
        let status = self.ivars().status.borrow();
        let links = self.ivars().links.borrow();
        let mut copy = status_copy(&status, &links);
        if self.ivars().adding.get() && matches!(*status, Status::Connected { .. }) {
            copy.detail = "Waiting for a new system. Click Start Sharing on it.".to_owned();
        }
        if let Some(views) = self.ivars().main.get()
            && let Some(me) = self.ivars().me.get()
        {
            let mut names: std::collections::BTreeMap<_, _> = self
                .ivars()
                .peers
                .borrow()
                .iter()
                .map(|peer| (peer.key, peer.name.clone()))
                .collect();
            if let Status::Connected { peers } = &*status {
                names.extend(peers.iter().map(|(name, key)| (*key, name.clone())));
            }
            names.insert(me, self.ivars().local_name.borrow().clone());
            let displays = crate::macos::displays().unwrap_or_default();
            let arranged = self.ivars().arranged.borrow();
            let shown = map::scene(me, &names, arranged.as_ref(), &displays, &links);
            views.title.setStringValue(&NSString::from_str(&copy.title));
            views.detail.setStringValue(&NSString::from_str(&copy.detail));
            views.arrange.show(shown);
        }
        if let Some(item) = self.ivars().status_item.get()
            && let Some(button) = item.button(self.mtm())
        {
            self.set_status_image(&button, copy.connected);
        }
    }

    /// Opens this group to a new system for a short while, from the menu.
    fn add_a_system(&self) {
        let always = self.ivars().settings.borrow().always_discoverable && self.ivars().always_allowed.get();
        if !crate::setup::can_add_system(
            is_active(&self.ivars().status.borrow()),
            always,
            self.ivars().adding.get(),
        ) {
            return;
        }
        let _ = self.ivars().controller.send(Command::AddSystem);
        if let Some(panel) = self.ivars().pairing.get() {
            let until = std::time::Instant::now() + controller::ADD_SYSTEM_WINDOW;
            panel.show(panel::State::Waiting { until }, true);
        }
    }

    /// Keeps the pairing panel current: its countdown, and closing
    /// Connected on its own.
    fn tick_pairing(&self) {
        let Some(panel) = self.ivars().pairing.get() else {
            return;
        };
        panel.tick();
        // an exchange that ended some other way leaves nothing to wait for
        let stale = match panel.state() {
            // no session reports the window closing for a typed address
            Some(panel::State::Waiting { until }) if std::time::Instant::now() >= until + WAITING_GRACE => {
                panel.show(panel::State::NoneJoined, false);
                false
            }
            Some(panel::State::Checking { .. }) => panel.age() >= CHECKING_SHOWN,
            Some(panel::State::ShowCode { .. } | panel::State::EnterCode { .. }) => panel.age() >= CODE_SHOWN,
            _ => false,
        };
        if stale {
            panel.close();
        }
        let shown_for = self.ivars().connected_at.get().map(|at| at.elapsed());
        if shown_for.is_some_and(|shown| shown >= CONNECTED_SHOWN) {
            self.ivars().connected_at.set(None);
            if matches!(panel.state(), Some(panel::State::Connected { .. })) && !panel.panel.isKeyWindow() {
                panel.close();
            }
        }
        if let Some(confirmed) = self
            .ivars()
            .owner_confirmed
            .lock()
            .ok()
            .and_then(|mut result| result.take())
        {
            if confirmed {
                self.set_always_discoverable(true);
            }
            self.refresh_always_discoverable();
        }
    }

    fn confirm_always_discoverable(&self) -> bool {
        if !self.ivars().always_allowed.get() {
            self.show_alert(
                "Always Discoverable is turned off by your organization",
                "Use Add a System in the menu bar to add a system.",
                NSAlertStyle::Informational,
            );
            return false;
        }
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(&NSString::from_str("Make this system always discoverable?"));
        alert.setInformativeText(&NSString::from_str(
            "While it is sharing, nearby systems running Daisy can ask to join without you clicking Add a System. They still need the code.",
        ));
        alert.addButtonWithTitle(&NSString::from_str("Turn On"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        alert.runModal() == NSAlertFirstButtonReturn
    }

    /// Asks for Touch ID or the login password. The answer arrives on
    /// another thread and is picked up by `tick_pairing`.
    fn confirm_owner(&self) {
        use objc2_local_authentication::{LAContext, LAPolicy};
        let result = self.ivars().owner_confirmed.clone();
        let reply = block2::RcBlock::new(
            move |ok: objc2::runtime::Bool, _error: *mut objc2_foundation::NSError| {
                if let Ok(mut slot) = result.lock() {
                    *slot = Some(ok.as_bool());
                }
            },
        );
        // SAFETY: the reply only stores a bool behind a mutex, so it may run
        // on any thread
        unsafe {
            LAContext::new().evaluatePolicy_localizedReason_reply(
                LAPolicy::DeviceOwnerAuthentication,
                &NSString::from_str("turn on Always Discoverable"),
                &reply,
            );
        }
    }

    fn set_always_discoverable(&self, on: bool) {
        self.ivars().settings.borrow_mut().always_discoverable = on;
        let _ = self.ivars().controller.send(Command::SetAlwaysDiscoverable(on));
        self.update_action_buttons();
    }

    fn refresh_always_discoverable(&self) {
        if let Some(views) = self.ivars().advanced.get() {
            let on = self.ivars().settings.borrow().always_discoverable && self.ivars().always_allowed.get();
            views.always.set_on(on);
        }
    }

    /// Asks on a sheet over `window` for a peer's address, then connects to it.
    fn ask_peer_address(&self, window: &NSWindow) {
        let mtm = self.mtm();
        let sheet = sheet::Sheet::new(mtm);
        let field = objc2_app_kit::NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        field.setPlaceholderString(Some(&NSString::from_str("Name or IP address")));
        field.setAccessibilityLabel(Some(&NSString::from_str("Peer address")));
        field.setFrame(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(340.0, 24.0)));
        let this = self.retain();
        let input = field.clone();
        sheet.present(
            window,
            "Add peer by address",
            "Enter the peer's local name or IP address.",
            &field,
            Some(&field),
            "Connect",
            move || {
                let address = input.stringValue().to_string();
                let current = this.ivars().settings.borrow().last_session.clone();
                let settings = crate::setup::connect_by_address(&current, &address)
                    .ok_or_else(|| "Enter a local name or IP address.".to_owned())?;
                this.connect_address(settings);
                Ok(())
            },
        );
    }

    fn connect_address(&self, settings: SessionSettings) {
        self.apply_settings(&settings);
        let _ = self.ivars().controller.send(Command::ConnectByAddress(settings));
    }

    fn start(&self) {
        if self.setup_step() != Step::Done {
            self.open_setup();
            return;
        }
        let settings = self.ivars().settings.borrow().last_session.clone();
        let _ = self.ivars().controller.send(Command::Start {
            settings,
            side_chosen: false,
        });
    }

    fn apply_settings(&self, settings: &SessionSettings) {
        self.ivars().settings.borrow_mut().last_session = settings.clone();
    }

    fn refresh_permissions(&self) {
        let accessibility = permissions::accessibility();
        let input = permissions::input_monitoring();
        if let Some(views) = self.ivars().advanced.get() {
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
        if let Some(views) = self.ivars().main.get() {
            views.start.setEnabled(can_start);
            views.start.setHidden(active);
            views.start.setKeyEquivalent(&NSString::from_str("\r"));
            self.style_action_button(&views.start, true);
            views.stop.setEnabled(active);
            views.stop.setHidden(!active);
        }
        if let Some(item) = self.ivars().menu_start_stop.get() {
            item.setEnabled(active || can_start);
            item.setTitle(&NSString::from_str(if active {
                "Stop Sharing"
            } else {
                "Start Sharing"
            }));
        }
        if let Some(item) = self.ivars().menu_add.get() {
            let always = self.ivars().settings.borrow().always_discoverable && self.ivars().always_allowed.get();
            item.setEnabled(crate::setup::can_add_system(active, always, self.ivars().adding.get()));
        }
    }

    fn rebuild_peers_list(&self) {
        let Some(views) = self.ivars().main.get() else {
            return;
        };
        let peers = self.ivars().peers.borrow();
        let links = self.ivars().links.borrow();
        let now = crate::trust::now();
        let rows: Vec<window::PeerRow> = peers
            .iter()
            .map(|peer| {
                let introducer = peer
                    .introduced_by
                    .and_then(|key| peers.iter().find(|other| other.key == key))
                    .map(|other| other.name.as_str());
                let live = links.get(&peer.key).is_some();
                window::PeerRow {
                    name: peer.name.clone(),
                    detail: format!(
                        "{} · {}",
                        peer_detail(introducer, links.get(&peer.key)),
                        expiry_caption(peer.policy.expiry(peer.paired_at, peer.last_seen, live, now))
                    ),
                    fingerprint: peer.key.fingerprint(),
                    trust: peer.policy.label(),
                }
            })
            .collect();
        views.show_peers(&rows, self);
    }

    fn refresh_share_clipboard(&self) {
        if let Some(views) = self.ivars().advanced.get() {
            views.clipboard.set_on(self.ivars().settings.borrow().share_clipboard);
        }
    }

    fn refresh_update_policy(&self) {
        if let Some(views) = self.ivars().advanced.get() {
            let automatic = self.ivars().settings.borrow().check_updates;
            views.check_updates.set_on(automatic);
            views.update_policy.setEnabled(automatic);
            let policy = self.ivars().settings.borrow().update_policy;
            if let Some(index) = crate::update::ORDER.iter().position(|candidate| *candidate == policy) {
                views.update_policy.selectItemAtIndex(index as isize);
            }
            let caption = if automatic {
                policy.caption()
            } else {
                "Daisy checks only when you click Check Now."
            };
            views.update_caption.setStringValue(&NSString::from_str(caption));
        }
    }

    /// Registers Daisy to open at login; on by default, so done on first run.
    fn open_at_login(&self) {
        // SAFETY: a plain status query and registration
        let service = unsafe { SMAppService::mainAppService() };
        if unsafe { service.status() } != SMAppServiceStatus::Enabled
            && let Err(error) = unsafe { service.registerAndReturnError() }
        {
            tracing::warn!(error = %error.localizedDescription(), "open at login could not be turned on");
        }
        self.refresh_launch_at_login();
    }

    fn refresh_launch_at_login(&self) {
        let Some(views) = self.ivars().advanced.get() else {
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
        // the arrangement would otherwise take focus and show its focus ring
        // to people not using the keyboard; Tab still reaches it
        window.makeFirstResponder(None);
        app.activate();
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
    let _focus = crate::macos::diagnostics::FocusObserver::observe();
    let mtm = MainThreadMarker::new().context("Daisy's interface must start on the main thread")?;
    crate::macos::file_pasteboard::enable(mtm);
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

/// The line under a peer's name: its fingerprint, who introduced it, and
/// how its link is doing while it runs.
fn peer_detail(introducer: Option<&str>, link: Option<&Link>) -> String {
    let mut parts = Vec::new();
    if let Some(introducer) = introducer {
        parts.push(format!("via {introducer}"));
    }
    match link {
        Some(link) if link.peer_locked => parts.push("Locked".to_owned()),
        Some(Link {
            latency_ms: Some(ms), ..
        }) => parts.push(format!("Connected, {ms} ms")),
        Some(_) => parts.push("Connected".to_owned()),
        None => parts.push("Not connected".to_owned()),
    }
    parts.join(" · ")
}

/// The line under the trust button: when it lapses, or why it does not.
fn expiry_caption(expiry: crate::trust::Expiry) -> String {
    use crate::trust::Expiry;
    match expiry {
        Expiry::Expired => "Expired".to_owned(),
        Expiry::At(at) => format!("Until {}", format_timestamp(at)),
        Expiry::Never => "Remembered forever".to_owned(),
        Expiry::SessionEnd => "Until this session ends".to_owned(),
        Expiry::RenewsWhileConnected => "Renews while connected".to_owned(),
    }
}

fn format_time(at: crate::trust::Timestamp) -> String {
    let formatter = NSDateFormatter::new();
    formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str("jmm"));
    formatter
        .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(at as f64))
        .to_string()
}

fn format_timestamp(at: crate::trust::Timestamp) -> String {
    let date = NSDate::dateWithTimeIntervalSince1970(at as f64);
    let formatter = NSDateFormatter::new();
    let this_year = NSDateFormatter::new();
    this_year.setLocalizedDateFormatFromTemplate(&NSString::from_str("y"));
    let same_year = this_year.stringFromDate(&date) == this_year.stringFromDate(&NSDate::new());
    let template = if same_year { "MMMdjmm" } else { "yMMMdjmm" };
    formatter.setLocalizedDateFormatFromTemplate(&NSString::from_str(template));
    formatter.stringFromDate(&date).to_string()
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

/// `links` is each running link's latest report.
fn status_copy(status: &Status, links: &std::collections::BTreeMap<crate::identity::PublicKey, Link>) -> StatusCopy {
    let plain = |title: String, detail: String| StatusCopy {
        menu: title.clone(),
        title,
        detail,
        connected: false,
    };
    match status {
        Status::Idle => plain(
            "Not Sharing".to_owned(),
            "Click Start Sharing to use this keyboard on other systems.".to_owned(),
        ),
        Status::Starting => plain("Starting…".to_owned(), "Daisy is preparing the connection.".to_owned()),
        Status::Waiting {
            pairing, looking_for, ..
        } => plain(
            match (pairing, looking_for) {
                (true, _) => "Looking for another system".to_owned(),
                (false, Some(peer)) => format!("Looking for {peer}"),
                (false, None) => "Looking for peers".to_owned(),
            },
            if *pairing {
                "Click Start Sharing on it.".to_owned()
            } else {
                "Daisy connects when a peer is on this network.".to_owned()
            },
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
        Status::Connected { peers } => {
            let names: Vec<&str> = peers.iter().map(|(name, _)| name.as_str()).collect();
            let title = match names.as_slice() {
                [] => "Connected".to_owned(),
                [one] => format!("Connected to {one}"),
                [first, second] => format!("Connected to {first} and {second}"),
                [first, rest @ ..] => format!("Connected to {first} and {} others", rest.len()),
            };
            let latencies: Vec<String> = peers
                .iter()
                .filter_map(|(name, key)| {
                    let ms = links.get(key)?.latency_ms?;
                    Some(if peers.len() == 1 {
                        format!("{ms} ms")
                    } else {
                        format!("{name} {ms} ms")
                    })
                })
                .collect();
            let locked: Vec<&str> = peers
                .iter()
                .filter(|(_, key)| links.get(key).is_some_and(|link| link.peer_locked))
                .map(|(name, _)| name.as_str())
                .collect();
            let menu = match latencies.as_slice() {
                [] => title.clone(),
                latencies => format!("{title} · {}", latencies.join(", ")),
            };
            let detail = match locked.as_slice() {
                [] => latencies.join(" · "),
                [one] => format!("{one} is locked; unlock it there."),
                many => format!("{} are locked; unlock them there.", many.join(", ")),
            };
            StatusCopy {
                title,
                detail,
                menu,
                connected: true,
            }
        }
        Status::Problem { summary, recovery } => plain(summary.clone(), recovery.clone()),
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

    fn key(byte: u8) -> crate::identity::PublicKey {
        crate::identity::PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    fn connected(names: &[&str]) -> Status {
        Status::Connected {
            peers: names
                .iter()
                .enumerate()
                .map(|(index, name)| ((*name).to_owned(), key(index as u8 + 1)))
                .collect(),
        }
    }

    fn measured(ms: Option<u64>, locked: bool) -> Link {
        Link {
            latency_ms: ms,
            peer_locked: locked,
            ..Link::default()
        }
    }

    #[test]
    fn the_menu_names_the_group_and_its_round_trips() {
        let links = std::collections::BTreeMap::from([(key(1), measured(Some(4), false))]);
        let copy = status_copy(&connected(&["Studio"]), &links);
        assert!(copy.connected);
        assert_eq!(copy.title, "Connected to Studio");
        assert_eq!(copy.menu, "Connected to Studio · 4 ms");
        assert_eq!(
            status_copy(&connected(&["Studio"]), &Default::default()).menu,
            "Connected to Studio"
        );

        let links =
            std::collections::BTreeMap::from([(key(1), measured(Some(4), false)), (key(2), measured(Some(12), false))]);
        let copy = status_copy(&connected(&["Studio", "Desk"]), &links);
        assert_eq!(copy.title, "Connected to Studio and Desk");
        assert_eq!(copy.menu, "Connected to Studio and Desk · Studio 4 ms, Desk 12 ms");
        let copy = status_copy(&connected(&["Studio", "Desk", "Den"]), &Default::default());
        assert_eq!(copy.title, "Connected to Studio and 2 others");
    }

    #[test]
    fn a_peer_row_says_who_introduced_it_and_how_its_link_is() {
        assert_eq!(peer_detail(None, None), "Not connected");
        assert_eq!(
            peer_detail(Some("Laptop"), Some(&measured(Some(12), false))),
            "via Laptop · Connected, 12 ms"
        );
        assert_eq!(peer_detail(None, Some(&measured(None, true))), "Locked");
    }

    #[test]
    fn a_locked_member_says_how_to_unlock_it() {
        let links = std::collections::BTreeMap::from([(key(2), measured(None, true))]);
        let copy = status_copy(&connected(&["Studio", "Desk"]), &links);
        assert_eq!(copy.detail, "Desk is locked; unlock it there.");
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
                &Default::default(),
            )
            .title
        };
        assert_eq!(waiting(true, Some("Studio")), "Looking for another system");
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
            &Default::default(),
        );
        assert_eq!(connecting.title, "Connecting to Studio…");
        let reconnecting = status_copy(
            &Status::Reconnecting {
                address: "192.168.1.20:24850".to_owned(),
                peer: Some("Studio".to_owned()),
                wait: std::time::Duration::from_secs(2),
            },
            &Default::default(),
        );
        assert_eq!(reconnecting.title, "Reconnecting to Studio…");
        assert!(reconnecting.detail.starts_with("The connection to Studio was lost."));
        let unknown = status_copy(
            &Status::Connecting {
                address: "192.168.1.20:24850".to_owned(),
                peer: None,
            },
            &Default::default(),
        );
        assert_eq!(unknown.title, "Connecting…");
        assert_eq!(status_copy(&Status::Idle, &Default::default()).title, "Not Sharing");
    }
}
