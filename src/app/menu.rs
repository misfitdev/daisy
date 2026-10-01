//! Menus whose enabled states are owned by Daisy.

use objc2::{MainThreadOnly, rc::Retained};
use objc2_app_kit::NSMenu;
use objc2_foundation::{MainThreadMarker, NSString};

pub fn new(title: &str, mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &NSString::from_str(title));
    // AppKit's automatic validation disables delegate actions during modal alerts.
    // Daisy explicitly enables actions, including Stop and Quit, in every menu.
    menu.setAutoenablesItems(false);
    menu
}
