//! The one small panel pairing uses, on both systems: waiting for a new
//! system, the code to type or a field to type it in, then Connected. It
//! floats without making Daisy the active app, so a prompt from a nearby
//! system never takes the keyboard from whatever is in use.

use std::time::Instant;

use objc2::MainThreadOnly;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::sel;
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFloatingWindowLevel, NSFont, NSFontWeightRegular, NSLineBreakMode,
    NSPanel, NSTextAlignment, NSTextField, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use super::coral_color;
use crate::pairing::PairingCode;

const WIDTH: f64 = 380.0;
const HEIGHT: f64 = 176.0;
const MARGIN: f64 = 20.0;

/// What the panel shows.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Add a System is open until `until`.
    Waiting {
        until: Instant,
    },
    /// No system joined before Add a System closed.
    NoneJoined,
    /// This system was just added: the code to type on `peer`.
    ShowCode {
        peer: String,
        code: PairingCode,
    },
    /// This system was already there: type the code shown on `peer`.
    EnterCode {
        peer: String,
        problem: Option<String>,
    },
    /// The code was sent; waiting to hear whether it matched.
    Checking {
        peer: String,
    },
    /// The code typed did not match.
    Mismatch,
    Connected {
        peer: String,
    },
}

pub struct Panel {
    pub panel: Retained<NSPanel>,
    title: Retained<NSTextField>,
    code: Retained<NSTextField>,
    pub field: Retained<NSTextField>,
    detail: Retained<NSTextField>,
    primary: Retained<NSButton>,
    secondary: Retained<NSButton>,
    state: std::cell::RefCell<Option<State>>,
    /// When the current kind of state began.
    since: std::cell::Cell<Instant>,
}

impl Panel {
    /// Buttons send `pairingPrimary:` and `pairingSecondary:` to `target`.
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect(0.0, 0.0, WIDTH, HEIGHT),
            NSWindowStyleMask::Titled | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        // SAFETY: Panel holds the panel, so AppKit must not release it on close
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setTitle(&NSString::from_str("Daisy"));
        panel.setLevel(NSFloatingWindowLevel);
        panel.setHidesOnDeactivate(false);
        panel.setBecomesKeyOnlyIfNeeded(true);
        let root = super::window::Panel::new(mtm, rect(0.0, 0.0, WIDTH, HEIGHT), None, 0.0);
        panel.setContentView(Some(&root));
        let inner = WIDTH - 2.0 * MARGIN;

        let title = label("", NSFont::boldSystemFontOfSize(15.0), mtm);
        title.setFrame(rect(MARGIN, MARGIN, inner, 22.0));
        root.addSubview(&title);

        // SAFETY (both fonts below): AppKit's font weight constant
        let code = label(
            "",
            NSFont::monospacedDigitSystemFontOfSize_weight(30.0, unsafe { NSFontWeightRegular }),
            mtm,
        );
        code.setAlignment(NSTextAlignment::Center);
        code.setSelectable(true);
        code.setFrame(rect(MARGIN, 50.0, inner, 38.0));
        root.addSubview(&code);

        let field = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
        field.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(22.0, unsafe {
            NSFontWeightRegular
        })));
        field.setAlignment(NSTextAlignment::Center);
        field.setPlaceholderString(Some(&NSString::from_str("6 digits")));
        field.setFrame(rect((WIDTH - 180.0) / 2.0, 52.0, 180.0, 34.0));
        // SAFETY: target implements pairingPrimary: and outlives the panel
        unsafe {
            field.setTarget(Some(target));
            field.setAction(Some(sel!(pairingPrimary:)));
        }
        root.addSubview(&field);

        let detail = label("", NSFont::systemFontOfSize(12.0), mtm);
        detail.setTextColor(Some(&NSColor::secondaryLabelColor()));
        detail.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        detail.setFrame(rect(MARGIN, 94.0, inner, 32.0));
        root.addSubview(&detail);

        let primary = button("", sel!(pairingPrimary:), target, mtm);
        primary.setBezelColor(Some(&coral_color()));
        primary.setContentTintColor(Some(&NSColor::whiteColor()));
        primary.setFrame(rect(WIDTH - MARGIN - 110.0, HEIGHT - MARGIN - 28.0, 110.0, 28.0));
        root.addSubview(&primary);
        let secondary = button("", sel!(pairingSecondary:), target, mtm);
        secondary.setKeyEquivalent(&NSString::from_str("\u{1b}"));
        secondary.setFrame(rect(WIDTH - MARGIN - 220.0, HEIGHT - MARGIN - 28.0, 100.0, 28.0));
        root.addSubview(&secondary);

        Self {
            panel,
            title,
            code,
            field,
            detail,
            primary,
            secondary,
            state: std::cell::RefCell::new(None),
            since: std::cell::Cell::new(Instant::now()),
        }
    }

    pub fn state(&self) -> Option<State> {
        self.state.borrow().clone()
    }

    /// Shows `state`. The panel takes the keyboard only when `focus`.
    pub fn show(&self, state: State, focus: bool) {
        let (title, detail, primary, secondary): (String, String, Option<&str>, Option<&str>) = match &state {
            State::Waiting { until } => (
                "Waiting for a new system".to_owned(),
                format!(
                    "Click Start Sharing on it. Its code will appear there. {}",
                    countdown(until.saturating_duration_since(Instant::now()).as_secs())
                ),
                None,
                Some("Cancel"),
            ),
            State::NoneJoined => (
                "No new system joined".to_owned(),
                "Click Start Sharing on it first, then try again here.".to_owned(),
                Some("Try Again"),
                Some("Close"),
            ),
            State::ShowCode { peer, .. } => (
                format!("Type this code on {peer}"),
                format!("{peer} is asking to connect."),
                None,
                Some("Cancel"),
            ),
            State::EnterCode { peer, problem } => (
                format!("Enter the code shown on {peer}"),
                problem.clone().unwrap_or_default(),
                Some("Connect"),
                Some("Cancel"),
            ),
            State::Checking { peer } => (format!("Connecting to {peer}…"), String::new(), None, None),
            State::Mismatch => (
                "That code didn't match".to_owned(),
                "Daisy asks again with a new code in a moment.".to_owned(),
                None,
                Some("Close"),
            ),
            State::Connected { peer } => (
                format!("Connected to {peer}"),
                "Drag its screen in Daisy to match your desk.".to_owned(),
                Some("Arrange…"),
                Some("Close"),
            ),
        };
        let entering = matches!(state, State::EnterCode { .. });
        if entering && !matches!(self.state(), Some(State::EnterCode { .. })) {
            self.field.setStringValue(&NSString::from_str(""));
        }
        self.title.setStringValue(&NSString::from_str(&title));
        self.detail.setStringValue(&NSString::from_str(&detail));
        if let State::ShowCode { code, .. } = &state {
            self.code.setStringValue(&NSString::from_str(&spaced(code)));
        }
        self.code.setHidden(!matches!(state, State::ShowCode { .. }));
        self.field.setHidden(!entering);
        for (button, title) in [(&self.primary, primary), (&self.secondary, secondary)] {
            button.setHidden(title.is_none());
            button.setTitle(&NSString::from_str(title.unwrap_or_default()));
        }
        self.primary
            .setKeyEquivalent(&NSString::from_str(if primary.is_some() { "\r" } else { "" }));
        let same_kind = self
            .state()
            .is_some_and(|shown| std::mem::discriminant(&shown) == std::mem::discriminant(&state));
        if !same_kind {
            self.since.set(Instant::now());
        }
        *self.state.borrow_mut() = Some(state);
        if !self.panel.isVisible() {
            self.panel.center();
        }
        if focus {
            self.panel.makeKeyAndOrderFront(None);
            if entering {
                self.panel.makeFirstResponder(Some(&self.field));
            }
        } else {
            self.panel.orderFrontRegardless();
        }
    }

    /// Updates the Add a System countdown, if that is what is shown.
    pub fn tick(&self) {
        if let Some(State::Waiting { until }) = self.state() {
            let left = until.saturating_duration_since(Instant::now()).as_secs();
            self.detail.setStringValue(&NSString::from_str(&format!(
                "Click Start Sharing on it. Its code will appear there. {}",
                countdown(left)
            )));
        }
    }

    /// How long the current kind of state has been shown.
    pub fn age(&self) -> std::time::Duration {
        self.since.get().elapsed()
    }

    pub fn close(&self) {
        *self.state.borrow_mut() = None;
        self.panel.orderOut(None);
    }
}

/// "482 913": two groups read and type more easily than six digits.
fn spaced(code: &PairingCode) -> String {
    let digits = code.to_string().replace(['-', ' '], "");
    match digits.len() {
        6 => format!("{} {}", &digits[..3], &digits[3..]),
        _ => digits,
    }
}

fn countdown(seconds: u64) -> String {
    format!("{}:{:02} left.", seconds / 60, seconds % 60)
}

fn label(text: &str, font: Retained<NSFont>, mtm: MainThreadMarker) -> Retained<NSTextField> {
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&font));
    label
}

fn button(title: &str, action: Sel, target: &AnyObject, mtm: MainThreadMarker) -> Retained<NSButton> {
    // SAFETY: target implements action and outlives the panel
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(target), Some(action), mtm) }
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_countdown_reads_as_minutes_and_seconds() {
        assert_eq!(super::countdown(29), "0:29 left.");
        assert_eq!(super::countdown(90), "1:30 left.");
    }

    #[test]
    fn a_code_is_shown_in_two_groups() {
        let code = super::PairingCode::parse("482913").unwrap();
        assert_eq!(super::spaced(&code), "482 913");
    }
}
