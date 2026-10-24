//! Choosing how long this system trusts a peer.

use objc2::rc::Retained;
use objc2::{MainThreadOnly, sel};
use objc2_app_kit::{
    NSAccessibility, NSButton, NSControlStateValueOff, NSControlStateValueOn, NSPopUpButton, NSTextField, NSView,
    NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSString};

use super::frame;
use super::sheet::Sheet;
use crate::trust::{Choice, MAX_DAYS, Policy};

/// Asks on a sheet over `window` how long to trust a peer, starting from
/// `current`, and calls `chosen` once a different policy is saved.
pub fn present(window: &NSWindow, title: &str, current: Policy, chosen: impl Fn(Policy) + 'static) {
    let sheet = Sheet::new(window.mtm());
    let form = Form::new(window.mtm(), &sheet, current);
    let view = form.view.clone();
    sheet.present(
        window,
        title,
        "“Until unused for” counts time without a connection. Connecting resets that timer. When trust ends, pair again.",
        &view,
        None,
        "Save",
        move || match form.policy(current) {
            Some(policy) => {
                if policy != current {
                    chosen(policy);
                }
                Ok(())
            }
            None => Err(format!("Enter a duration from 1 hour to {MAX_DAYS} days.")),
        },
    );
}

struct Form {
    view: Retained<NSView>,
    session: Retained<NSButton>,
    forever: Retained<NSButton>,
    deadline: Option<Retained<NSButton>>,
    amount: Retained<NSTextField>,
    unit: Retained<NSPopUpButton>,
}

impl Form {
    fn new(mtm: MainThreadMarker, target: &Sheet, current: Policy) -> Self {
        let choice = Choice::of(current);
        let fixed = matches!(current, Policy::Days(_));
        let height = if fixed { 114.0 } else { 86.0 };
        let view = NSView::initWithFrame(NSView::alloc(mtm), frame(0.0, 0.0, 340.0, height));
        let radio = |title: &str, y: f64, on: bool| {
            // SAFETY: target implements choose: and outlives the sheet
            let button = unsafe {
                NSButton::radioButtonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(target),
                    Some(sel!(choose:)),
                    mtm,
                )
            };
            button.setFrame(frame(0.0, y, 160.0, 22.0));
            button.setState(if on {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
            view.addSubview(&button);
            button
        };
        let (amount, days) = match choice {
            Choice::Unused { amount, days } if !fixed => (amount, days),
            _ => (4, true),
        };
        let deadline = if let Policy::Days(days) = current {
            let title = format!("{days} {} from pairing", if days == 1 { "day" } else { "days" });
            let button = radio(&title, 92.0, true);
            button.setFrameSize(objc2_foundation::NSSize::new(340.0, 22.0));
            Some(button)
        } else {
            None
        };
        let session = radio("End of session", 64.0, choice == Choice::Session);
        let forever = radio("Forever", 36.0, choice == Choice::Forever);
        let unused = radio(
            "Until unused for",
            6.0,
            !fixed && matches!(choice, Choice::Unused { .. }),
        );
        unused.setFrame(frame(0.0, 6.0, 142.0, 22.0));

        let number = NSTextField::textFieldWithString(&NSString::from_str(&amount.to_string()), mtm);
        number.setFrame(frame(146.0, 4.0, 52.0, 24.0));
        number.setAccessibilityLabel(Some(&NSString::from_str("Time without a connection")));
        view.addSubview(&number);
        let unit =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame(204.0, 2.0, 96.0, 28.0), false);
        unit.addItemWithTitle(&NSString::from_str("hours"));
        unit.addItemWithTitle(&NSString::from_str("days"));
        unit.selectItemAtIndex(isize::from(days));
        unit.setAccessibilityLabel(Some(&NSString::from_str("Time unit")));
        view.addSubview(&unit);
        let amount_control = number.clone();
        let unit_control = unit.clone();
        target.on_choice(move || {
            let enabled = unused.state() == NSControlStateValueOn;
            amount_control.setEnabled(enabled);
            unit_control.setEnabled(enabled);
        });
        Self {
            view,
            session,
            forever,
            deadline,
            amount: number,
            unit,
        }
    }

    fn policy(&self, current: Policy) -> Option<Policy> {
        if self
            .deadline
            .as_ref()
            .is_some_and(|button| button.state() == NSControlStateValueOn)
        {
            return Some(current);
        }
        let choice = if self.session.state() == NSControlStateValueOn {
            Choice::Session
        } else if self.forever.state() == NSControlStateValueOn {
            Choice::Forever
        } else {
            Choice::Unused {
                amount: self.amount.stringValue().to_string().trim().parse().unwrap_or(0),
                days: self.unit.indexOfSelectedItem() == 1,
            }
        };
        choice.policy()
    }
}
