//! Choosing how long this system trusts a peer.

use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{MainThreadOnly, Message, sel};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAlertFirstButtonReturn, NSButton, NSControlStateValueOff, NSControlStateValueOn,
    NSModalResponse, NSPopUpButton, NSTextField, NSView, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSString};

use super::frame;
use crate::trust::{Choice, Policy};

/// Asks on a sheet over `window` how long to trust a peer, starting from
/// `current`, and calls `chosen` once a different policy is saved. An
/// invalid duration asks again. Radio buttons send `trustKind:` to
/// `target`, which AppKit needs to group them.
pub fn present(
    window: &NSWindow,
    target: Retained<NSObject>,
    title: Rc<str>,
    current: Policy,
    choice: Choice,
    chosen: Rc<dyn Fn(Policy)>,
) {
    let mtm = window.mtm();
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(&title));
    alert.setInformativeText(&NSString::from_str("When this ends, pair again to connect."));
    alert.addButtonWithTitle(&NSString::from_str("Save"));
    alert.addButtonWithTitle(&NSString::from_str("Cancel"));
    let form = Form::new(mtm, &target, choice);
    alert.setAccessoryView(Some(&form.view));
    let parent = window.retain();
    let handler = RcBlock::new(move |response: NSModalResponse| {
        if response != NSAlertFirstButtonReturn {
            return;
        }
        let picked = form.choice();
        match picked.confirmed(current) {
            Some(policy) if policy != current => chosen(policy),
            Some(_) => {}
            None => present(&parent, target.clone(), title.clone(), current, picked, chosen.clone()),
        }
    });
    alert.beginSheetModalForWindow_completionHandler(window, Some(&handler));
}

struct Form {
    view: Retained<NSView>,
    session: Retained<NSButton>,
    forever: Retained<NSButton>,
    amount: Retained<NSTextField>,
    unit: Retained<NSPopUpButton>,
}

impl Form {
    fn new(mtm: MainThreadMarker, target: &AnyObject, choice: Choice) -> Self {
        let view = NSView::initWithFrame(NSView::alloc(mtm), frame(0.0, 0.0, 300.0, 86.0));
        let radio = |title: &str, y: f64, on: bool| {
            // SAFETY: target implements trustKind: and outlives the alert
            let button = unsafe {
                NSButton::radioButtonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(target),
                    Some(sel!(trustKind:)),
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
            Choice::Unused { amount, days } => (amount, days),
            _ => (4, true),
        };
        let session = radio("End of session", 64.0, choice == Choice::Session);
        let forever = radio("Forever", 36.0, choice == Choice::Forever);
        let unused = radio("Duration", 6.0, matches!(choice, Choice::Unused { .. }));
        unused.setFrame(frame(0.0, 6.0, 132.0, 22.0));

        let number = NSTextField::textFieldWithString(&NSString::from_str(&amount.to_string()), mtm);
        number.setFrame(frame(136.0, 4.0, 52.0, 24.0));
        number.setAccessibilityLabel(Some(&NSString::from_str("Duration amount")));
        view.addSubview(&number);
        let unit =
            NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame(194.0, 2.0, 96.0, 28.0), false);
        unit.addItemWithTitle(&NSString::from_str("hours"));
        unit.addItemWithTitle(&NSString::from_str("days"));
        unit.selectItemAtIndex(isize::from(days));
        view.addSubview(&unit);
        Self {
            view,
            session,
            forever,
            amount: number,
            unit,
        }
    }

    fn choice(&self) -> Choice {
        if self.session.state() == NSControlStateValueOn {
            Choice::Session
        } else if self.forever.state() == NSControlStateValueOn {
            Choice::Forever
        } else {
            Choice::Unused {
                amount: self.amount.stringValue().to_string().trim().parse().unwrap_or(0),
                days: self.unit.indexOfSelectedItem() == 1,
            }
        }
    }
}
