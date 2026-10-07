//! Daisy's main window and its Advanced sheet, laid out like System
//! Settings: the arrangement on top, then rounded groups of rows.

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSFont, NSImage,
    NSImageView, NSLineBreakMode, NSResponder, NSTextField, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::coral_color;
use super::map::ArrangeView;
use super::switch::Switch;

pub const WIDTH: f64 = 560.0;
const MARGIN: f64 = 20.0;
const ROW: f64 = 38.0;
const PEER_ROW: f64 = 50.0;
const HERO: f64 = 214.0;
const GROUP_WIDTH: f64 = WIDTH - 2.0 * MARGIN;
const ADVANCED_WIDTH: f64 = 460.0;

pub struct PanelIvars {
    fill: RefCell<Option<Retained<NSColor>>>,
    radius: Cell<f64>,
}

define_class!(
    // SAFETY: NSView has no additional subclassing requirements; panels are
    // used only on the main thread.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PanelIvars]
    /// A view laid out from the top, optionally filled as a rounded group.
    pub struct Panel;

    unsafe impl NSObjectProtocol for Panel {}

    impl Panel {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if let Some(fill) = self.ivars().fill.borrow().as_ref() {
                fill.setFill();
                let radius = self.ivars().radius.get();
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.bounds(), radius, radius).fill();
            }
        }
    }
);

impl Panel {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, fill: Option<Retained<NSColor>>, radius: f64) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(PanelIvars {
            fill: RefCell::new(fill),
            radius: Cell::new(radius),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    fn group(mtm: MainThreadMarker) -> Retained<Self> {
        Self::new(
            mtm,
            rect(0.0, 0.0, GROUP_WIDTH, ROW),
            Some(NSColor::quaternarySystemFillColor()),
            10.0,
        )
    }

    fn clear(&self) {
        for view in self.subviews().iter() {
            view.removeFromSuperview();
        }
    }
}

pub struct MainViews {
    pub window: Retained<NSWindow>,
    root: Retained<Panel>,
    pub arrange: Retained<ArrangeView>,
    pub title: Retained<NSTextField>,
    pub detail: Retained<NSTextField>,
    peers_heading: Retained<NSTextField>,
    add_peer: Retained<NSButton>,
    peers: Retained<Panel>,
    buttons: Retained<Panel>,
    pub start: Retained<NSButton>,
    pub stop: Retained<NSButton>,
}

/// One paired peer, as its row shows it.
pub struct PeerRow {
    pub name: String,
    /// Who introduced it and how its link is doing.
    pub detail: String,
    /// Shown on hover and to VoiceOver, so it is not mistaken for a code.
    pub fingerprint: String,
    pub trust: String,
}

impl MainViews {
    /// Every control sends its action to `target`.
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        // SAFETY: plain values; the window is kept alive by MainViews
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, WIDTH, 600.0),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: MainViews holds the window, so AppKit must not release it on close
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Daisy"));
        let root = Panel::new(mtm, rect(0.0, 0.0, WIDTH, 600.0), None, 0.0);
        window.setContentView(Some(&root));

        let hero = Panel::new(
            mtm,
            rect(0.0, 0.0, WIDTH, HERO),
            Some(NSColor::quaternarySystemFillColor()),
            0.0,
        );
        root.addSubview(&hero);
        let arrange = ArrangeView::new(mtm, rect(MARGIN, 14.0, GROUP_WIDTH, 132.0), target, sel!(arrangePeer:));
        hero.addSubview(&arrange);
        let title = label("Not Sharing", 15.0, true, mtm);
        title.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        title.setFrame(rect(MARGIN, 152.0, GROUP_WIDTH, 22.0));
        hero.addSubview(&title);
        let detail = label("", 12.0, false, mtm);
        detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        detail.setAlignment(objc2_app_kit::NSTextAlignment::Center);
        detail.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        detail.setFrame(rect(MARGIN, 176.0, GROUP_WIDTH, 18.0));
        hero.addSubview(&detail);
        let rule = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, HERO, WIDTH, 1.0));
        rule.setBoxType(NSBoxType::Separator);
        root.addSubview(&rule);

        let peers_heading = label("Peers", 13.0, true, mtm);
        root.addSubview(&peers_heading);
        let add_peer = button("+", sel!(addPeerByAddress:), target, mtm);
        add_peer.setAccessibilityLabel(Some(&NSString::from_str("Add peer by address")));
        add_peer.setToolTip(Some(&NSString::from_str("Add peer by address")));
        root.addSubview(&add_peer);
        let peers = Panel::group(mtm);
        root.addSubview(&peers);

        let buttons = Panel::new(mtm, rect(MARGIN, 0.0, GROUP_WIDTH, 32.0), None, 0.0);
        root.addSubview(&buttons);
        let advanced = button("Advanced…", sel!(openAdvanced:), target, mtm);
        advanced.setFrame(rect(0.0, 0.0, 110.0, 32.0));
        buttons.addSubview(&advanced);
        let start = button("Start Sharing", sel!(startSession:), target, mtm);
        start.setFrame(rect(GROUP_WIDTH - 130.0, 0.0, 130.0, 32.0));
        buttons.addSubview(&start);
        let stop = button("Stop Sharing", sel!(stopSession:), target, mtm);
        stop.setFrame(rect(GROUP_WIDTH - 130.0, 0.0, 130.0, 32.0));
        stop.setHidden(true);
        buttons.addSubview(&stop);

        let views = Self {
            window,
            root,
            arrange,
            title,
            detail,
            peers_heading,
            add_peer,
            peers,
            buttons,
            start,
            stop,
        };
        views.root.addSubview(&escape_key(sel!(closeKeyWindow:), target, mtm));
        views.show_peers(&[], target);
        views.window.center();
        views
    }

    /// One row per peer, each with its trust and Forget buttons.
    pub fn show_peers(&self, peers: &[PeerRow], target: &AnyObject) {
        let mtm = self.window.mtm();
        self.peers.clear();
        if peers.is_empty() {
            let empty = label("Paired systems appear here.", 13.0, false, mtm);
            empty.setTextColor(Some(&NSColor::secondaryLabelColor()));
            empty.setFrame(rect(16.0, (ROW - 18.0) / 2.0, GROUP_WIDTH - 32.0, 18.0));
            self.peers.addSubview(&empty);
            self.peers.setFrameSize(NSSize::new(GROUP_WIDTH, ROW));
        }
        for (index, peer) in peers.iter().enumerate() {
            let y = index as f64 * PEER_ROW;
            if index > 0 {
                separator(&self.peers, y, mtm);
            }
            let name = label(&peer.name, 13.0, false, mtm);
            name.setToolTip(Some(&NSString::from_str(&format!(
                "Key fingerprint {}",
                peer.fingerprint
            ))));
            name.setAccessibilityHelp(Some(&NSString::from_str(&format!(
                "Key fingerprint {}",
                peer.fingerprint
            ))));
            name.setFrame(rect(16.0, y + 7.0, 200.0, 18.0));
            self.peers.addSubview(&name);
            let detail = label(&peer.detail, 11.0, false, mtm);
            detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
            detail.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            detail.setFrame(rect(16.0, y + 26.0, GROUP_WIDTH - 340.0, 16.0));
            self.peers.addSubview(&detail);
            let trust = button(&format!("{}…", peer.trust), sel!(changeTrust:), target, mtm);
            trust.setTag(index as isize);
            trust.setAccessibilityLabel(Some(&NSString::from_str(&format!(
                "Trust {}: {}",
                peer.name, peer.trust
            ))));
            trust.setFrame(rect(GROUP_WIDTH - 316.0, y + (PEER_ROW - 28.0) / 2.0, 210.0, 28.0));
            self.peers.addSubview(&trust);
            let forget = button("Forget…", sel!(forgetPeer:), target, mtm);
            forget.setTag(index as isize);
            forget.setAccessibilityLabel(Some(&NSString::from_str(&format!("Forget {}", peer.name))));
            forget.setFrame(rect(GROUP_WIDTH - 100.0, y + (PEER_ROW - 28.0) / 2.0, 84.0, 28.0));
            self.peers.addSubview(&forget);
        }
        if !peers.is_empty() {
            self.peers
                .setFrameSize(NSSize::new(GROUP_WIDTH, PEER_ROW * peers.len() as f64));
        }
        self.layout();
    }

    /// Stacks the groups from the top and fits the window to them, keeping
    /// its top edge where it is.
    fn layout(&self) {
        let mut y = HERO + MARGIN;
        let mut place = |view: &NSView, height: f64, after: f64| {
            view.setFrameOrigin(NSPoint::new(MARGIN, y));
            if view.frame().size.height != height {
                view.setFrameSize(NSSize::new(view.frame().size.width, height));
            }
            y += height + after;
        };
        place(&self.peers_heading, 24.0, 6.0);
        self.add_peer.setFrame(rect(
            MARGIN + GROUP_WIDTH - 28.0,
            self.peers_heading.frame().origin.y - 2.0,
            28.0,
            28.0,
        ));
        place(&self.peers, self.peers.frame().size.height, MARGIN);
        place(&self.buttons, 32.0, MARGIN);
        self.peers_heading.setFrameSize(NSSize::new(GROUP_WIDTH - 40.0, 24.0));

        let content = self.window.contentRectForFrameRect(self.window.frame());
        let top = content.origin.y + content.size.height;
        let resized = NSRect::new(NSPoint::new(content.origin.x, top - y), NSSize::new(WIDTH, y));
        self.window
            .setFrame_display(self.window.frameRectForContentRect(resized), self.window.isVisible());
        self.root.setNeedsDisplay(true);
    }
}

pub struct AdvancedViews {
    pub window: Retained<NSWindow>,
    pub always: Retained<Switch>,
    pub clipboard: Retained<Switch>,
    pub login: Retained<Switch>,
    pub update_policy: Retained<NSButton>,
    /// For Accessibility and Input Monitoring: shown when allowed, and the
    /// button that sets it up when not.
    permission_rows: [(Retained<NSView>, Retained<NSButton>); 2],
}

impl AdvancedViews {
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        let width = ADVANCED_WIDTH - 2.0 * MARGIN;
        // SAFETY: plain values; the window is kept alive by AdvancedViews
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0.0, 0.0, ADVANCED_WIDTH, 600.0),
                NSWindowStyleMask::Titled,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: AdvancedViews holds the window, so AppKit must not release it on close
        unsafe { window.setReleasedWhenClosed(false) };
        let root = Panel::new(mtm, rect(0.0, 0.0, ADVANCED_WIDTH, 600.0), None, 0.0);
        window.setContentView(Some(&root));
        let mut y = MARGIN;

        let heading = label("Advanced", 15.0, true, mtm);
        heading.setFrame(rect(MARGIN, y, width, 22.0));
        root.addSubview(&heading);
        y += 22.0 + 14.0;

        let options = Panel::new(
            mtm,
            rect(MARGIN, y, width, ROW),
            Some(NSColor::quaternarySystemFillColor()),
            10.0,
        );
        root.addSubview(&options);
        let switch = |on: bool, title: &str, action: Sel| Switch::new(mtm, on, target, action, title);
        let clipboard = switch(true, "Share clipboard when control moves", sel!(toggleShareClipboard:));
        let login = switch(true, "Open at login", sel!(toggleLaunchAtLogin:));
        let always = switch(false, "Always discoverable", sel!(toggleAlwaysDiscoverable:));
        let update_policy = button("Notify only", sel!(cycleUpdatePolicy:), target, mtm);
        update_policy.setAccessibilityLabel(Some(&NSString::from_str("Automatic updates policy")));
        update_policy.setToolTip(Some(&NSString::from_str("Click to choose the next update policy")));
        update_policy.setFrameSize(NSSize::new(210.0, 28.0));
        rows(
            &options,
            &[
                ("Share clipboard when control moves", &**clipboard),
                ("Open at login", &**login),
                ("Always discoverable", &**always),
                ("Automatic updates", &*update_policy),
            ],
            mtm,
        );
        y += options.frame().size.height + MARGIN;

        let permissions_heading = label("Permissions", 13.0, true, mtm);
        permissions_heading.setFrame(rect(MARGIN, y, width, 18.0));
        root.addSubview(&permissions_heading);
        y += 18.0 + 6.0;
        let permissions = Panel::new(
            mtm,
            rect(MARGIN, y, width, ROW),
            Some(NSColor::quaternarySystemFillColor()),
            10.0,
        );
        root.addSubview(&permissions);
        let permission_rows = ["Accessibility", "Input Monitoring"].map(|name| {
            let allowed = allowed_badge(mtm);
            let set_up = button("Set Up…", sel!(setUpPermissions:), target, mtm);
            set_up.setAccessibilityLabel(Some(&NSString::from_str(&format!("Set up {name}"))));
            let holder = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 120.0, 28.0));
            allowed.setFrame(rect(0.0, 4.0, 120.0, 20.0));
            set_up.setFrame(rect(30.0, 0.0, 90.0, 28.0));
            holder.addSubview(&allowed);
            holder.addSubview(&set_up);
            (holder, allowed, set_up)
        });
        let reset = button("Reset…", sel!(resetPermissions:), target, mtm);
        reset.setAccessibilityLabel(Some(&NSString::from_str("Reset permissions")));
        reset.setFrameSize(NSSize::new(84.0, 28.0));
        rows(
            &permissions,
            &[
                ("Accessibility", &*permission_rows[0].0),
                ("Input Monitoring", &*permission_rows[1].0),
                ("Reset permissions", &**reset),
            ],
            mtm,
        );
        let permission_rows = permission_rows.map(|(_, allowed, set_up)| (allowed, set_up));
        y += permissions.frame().size.height + MARGIN;

        let done = button("Done", sel!(closeAdvanced:), target, mtm);
        done.setKeyEquivalent(&NSString::from_str("\r"));
        done.setBezelColor(Some(&coral_color()));
        done.setContentTintColor(Some(&NSColor::whiteColor()));
        done.setFrame(rect(ADVANCED_WIDTH - MARGIN - 96.0, y, 96.0, 32.0));
        root.addSubview(&done);
        root.addSubview(&escape_key(sel!(closeAdvanced:), target, mtm));
        y += 32.0 + MARGIN;
        window.setContentSize(NSSize::new(ADVANCED_WIDTH, y));
        root.setFrameSize(NSSize::new(ADVANCED_WIDTH, y));

        Self {
            window,
            always,
            clipboard,
            login,
            update_policy,
            permission_rows,
        }
    }

    pub fn show_permissions(&self, allowed: [bool; 2]) {
        for ((badge, set_up), allowed) in self.permission_rows.iter().zip(allowed) {
            badge.setHidden(!allowed);
            set_up.setHidden(allowed);
        }
    }
}

/// Fills `group` with labelled rows, each with its control on the right.
fn rows(group: &Panel, entries: &[(&str, &NSView)], mtm: MainThreadMarker) {
    let width = group.frame().size.width;
    for (index, (title, control)) in entries.iter().enumerate() {
        let y = index as f64 * ROW;
        if index > 0 {
            separator(group, y, mtm);
        }
        let name = label(title, 13.0, false, mtm);
        name.setFrame(rect(16.0, y + (ROW - 18.0) / 2.0, width - 180.0, 18.0));
        group.addSubview(&name);
        let size = control.frame().size;
        control.setFrameOrigin(NSPoint::new(width - 16.0 - size.width, y + (ROW - size.height) / 2.0));
        group.addSubview(control);
    }
    group.setFrameSize(NSSize::new(width, ROW * entries.len() as f64));
}

fn separator(group: &NSView, y: f64, mtm: MainThreadMarker) {
    let line = NSBox::initWithFrame(NSBox::alloc(mtm), rect(16.0, y, group.frame().size.width - 32.0, 1.0));
    line.setBoxType(NSBoxType::Separator);
    group.addSubview(&line);
}

fn allowed_badge(mtm: MainThreadMarker) -> Retained<NSView> {
    let badge = NSView::initWithFrame(NSView::alloc(mtm), rect(0.0, 0.0, 120.0, 20.0));
    let text = label("Allowed", 13.0, false, mtm);
    text.setTextColor(Some(&NSColor::secondaryLabelColor()));
    text.setAlignment(objc2_app_kit::NSTextAlignment::Right);
    text.setFrame(rect(0.0, 1.0, 98.0, 18.0));
    badge.addSubview(&text);
    if let Some(check) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("checkmark.circle.fill"),
        Some(&NSString::from_str("Allowed")),
    ) {
        let image = NSImageView::imageViewWithImage(&check, mtm);
        image.setContentTintColor(Some(&NSColor::systemGreenColor()));
        image.setFrame(rect(102.0, 1.0, 18.0, 18.0));
        badge.addSubview(&image);
    }
    badge
}

/// Escape sends `action`. The main menu's Escape never reaches a window
/// while a text field is editing, but a button's key equivalent does; a
/// hidden button would not answer, so this one is transparent instead.
pub fn escape_key(action: Sel, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSButton> {
    let escape = button("", action, target, mtm);
    escape.setKeyEquivalent(&NSString::from_str("\u{1b}"));
    escape.setTransparent(true);
    escape.setRefusesFirstResponder(true);
    escape.setAccessibilityElement(false);
    escape.setFrame(rect(0.0, 0.0, 1.0, 1.0));
    escape
}

fn label(text: &str, size: f64, bold: bool, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    let font = if bold {
        NSFont::boldSystemFontOfSize(size)
    } else {
        NSFont::systemFontOfSize(size)
    };
    label.setFont(Some(&font));
    label
}

fn button(title: &str, action: Sel, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: target implements action and outlives the window
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm) }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}
