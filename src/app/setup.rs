//! The permission walkthrough window.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSButton, NSColor, NSFont, NSImage, NSImageScaling, NSImageView, NSTextField,
    NSTitlePosition, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSString};

use super::{coral_color, flower_image, frame};
use crate::setup::{Row, Step};

const WIDTH: f64 = 460.0;
const HEIGHT: f64 = 310.0;

pub struct SetupViews {
    pub window: Retained<NSWindow>,
    rows: [RowViews; 2],
    note: Retained<NSTextField>,
    action: Retained<NSButton>,
}

struct RowViews {
    permission: Step,
    number: &'static str,
    badge: Retained<NSImageView>,
    allow: Retained<NSButton>,
}

impl SetupViews {
    /// Buttons send `setupAllow:` and `setupAction:` to `target`.
    pub fn new(mtm: MainThreadMarker, target: &AnyObject) -> Self {
        // SAFETY: plain values; the window is kept alive by SetupViews
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame(0.0, 0.0, WIDTH, HEIGHT),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // SAFETY: SetupViews holds the window, so AppKit must not release it on close
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&NSString::from_str("Set Up Daisy"));
        window.center();
        let content = window.contentView().expect("window has a content view");

        let flower = NSImageView::imageViewWithImage(
            &flower_image(true, 44.0).expect("Daisy's bundled mark is valid SVG"),
            mtm,
        );
        flower.setFrame(frame(28.0, HEIGHT - 72.0, 44.0, 44.0));
        content.addSubview(&flower);
        let heading = label("Allow Daisy to share input", 17.0, true, mtm);
        heading.setFrame(frame(86.0, HEIGHT - 50.0, WIDTH - 114.0, 24.0));
        content.addSubview(&heading);
        let subheading = label("macOS needs your OK on each system.", 13.0, false, mtm);
        subheading.setTextColor(Some(&NSColor::secondaryLabelColor()));
        subheading.setFrame(frame(86.0, HEIGHT - 72.0, WIDTH - 114.0, 20.0));
        content.addSubview(&subheading);

        let rows = [
            (
                Step::Accessibility,
                "1",
                "Accessibility",
                "Move the pointer and type here.",
                HEIGHT - 162.0,
            ),
            (
                Step::InputMonitoring,
                "2",
                "Input Monitoring",
                "Read this keyboard and trackpad.",
                HEIGHT - 234.0,
            ),
        ]
        .map(|(permission, number, title, detail, y)| {
            let group = NSBox::initWithFrame(NSBox::alloc(mtm), frame(28.0, y, WIDTH - 56.0, 64.0));
            group.setBoxType(NSBoxType::Custom);
            group.setTitlePosition(NSTitlePosition::NoTitle);
            group.setTransparent(false);
            group.setBorderWidth(0.0);
            group.setCornerRadius(12.0);
            group.setFillColor(&NSColor::tertiarySystemFillColor());
            content.addSubview(&group);

            let badge = NSImageView::new(mtm);
            badge.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
            badge.setFrame(frame(44.0, y + 20.0, 24.0, 24.0));
            content.addSubview(&badge);
            let name = label(title, 13.0, true, mtm);
            name.setFrame(frame(80.0, y + 33.0, 220.0, 18.0));
            content.addSubview(&name);
            let explanation = label(detail, 12.0, false, mtm);
            explanation.setTextColor(Some(&NSColor::secondaryLabelColor()));
            explanation.setFrame(frame(80.0, y + 13.0, 220.0, 18.0));
            content.addSubview(&explanation);
            let allow = button("Allow…", sel!(setupAllow:), target, mtm);
            allow.setBezelColor(Some(&coral_color()));
            allow.setContentTintColor(Some(&NSColor::whiteColor()));
            allow.setFrame(frame(WIDTH - 140.0, y + 16.0, 96.0, 32.0));
            content.addSubview(&allow);
            RowViews {
                permission,
                number,
                badge,
                allow,
            }
        });

        let note = label("", 12.0, false, mtm);
        note.setTextColor(Some(&NSColor::secondaryLabelColor()));
        note.setFrame(frame(28.0, 26.0, WIDTH - 200.0, 18.0));
        content.addSubview(&note);
        let action = button("Continue", sel!(setupAction:), target, mtm);
        action.setFrame(frame(WIDTH - 156.0, 18.0, 128.0, 32.0));
        action.setBezelColor(Some(&coral_color()));
        action.setContentTintColor(Some(&NSColor::whiteColor()));
        content.addSubview(&action);
        content.addSubview(&super::window::escape_key(sel!(closeKeyWindow:), target, mtm));

        Self {
            window,
            rows,
            note,
            action,
        }
    }

    /// `asked` is whether the person was sent to System Settings for the
    /// current step.
    pub fn show(&self, step: Step, asked: bool) {
        for row in &self.rows {
            let state = crate::setup::row(step, row.permission);
            let (symbol, tint) = match state {
                Row::Allowed => ("checkmark.circle.fill", NSColor::systemGreenColor()),
                Row::Current => (row.number, coral_color()),
                Row::Later => (row.number, NSColor::tertiaryLabelColor()),
            };
            let symbol = if state == Row::Allowed {
                symbol.to_owned()
            } else {
                format!("{symbol}.circle")
            };
            row.badge.setImage(
                NSImage::imageWithSystemSymbolName_accessibilityDescription(
                    &NSString::from_str(&symbol),
                    Some(&NSString::from_str(match state {
                        Row::Allowed => "Allowed",
                        Row::Current | Row::Later => "Not allowed yet",
                    })),
                )
                .as_deref(),
            );
            row.badge.setContentTintColor(Some(&tint));
            // never on Return: a stray keypress must not open a permission prompt
            row.allow.setHidden(state != Row::Current);
        }
        let (note, action) = match step {
            Step::Accessibility | Step::InputMonitoring if asked => ("Switch Daisy on in System Settings.", None),
            Step::Accessibility | Step::InputMonitoring => ("", None),
            Step::Reopen => ("Input Monitoring starts after Daisy reopens.", Some("Reopen Daisy")),
            Step::Done => ("", Some("Continue")),
        };
        self.note.setStringValue(&NSString::from_str(note));
        self.action.setHidden(action.is_none());
        if let Some(title) = action {
            self.action.setTitle(&NSString::from_str(title));
            self.action.setKeyEquivalent(&NSString::from_str("\r"));
        } else {
            self.action.setKeyEquivalent(&NSString::from_str(""));
        }
    }
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
