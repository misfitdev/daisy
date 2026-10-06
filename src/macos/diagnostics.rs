//! Foreground-application observations for developer traces.

use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSWorkspace, NSWorkspaceDidActivateApplicationNotification};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol};

pub struct FocusObserver {
    center: Retained<NSNotificationCenter>,
    token: Retained<ProtocolObject<dyn NSObjectProtocol>>,
}

impl FocusObserver {
    pub fn observe() -> Self {
        let center = NSWorkspace::sharedWorkspace().notificationCenter();
        let block = block2::RcBlock::new(|_notification: NonNull<NSNotification>| {
            let pid = NSWorkspace::sharedWorkspace()
                .frontmostApplication()
                .map(|app| app.processIdentifier());
            tracing::debug!(pid = ?pid, "foreground application changed");
        });
        // SAFETY: the block does not retain or dereference notification data;
        // the center copies it and the observer is removed on drop.
        let token = unsafe {
            center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidActivateApplicationNotification),
                None,
                None,
                &block,
            )
        };
        Self { center, token }
    }
}

impl Drop for FocusObserver {
    fn drop(&mut self) {
        // SAFETY: this token belongs to this notification center.
        unsafe { self.center.removeObserver(self.token.as_ref()) };
    }
}
