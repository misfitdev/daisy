//! Pairing requests in macOS Notification Center.

use block2::RcBlock;
use objc2_foundation::{NSArray, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const ID: &str = "dev.misfit.daisy.pairing-request";

pub fn clear() {
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    if cfg!(test) {
        return;
    }
    remove_request(&UNUserNotificationCenter::currentNotificationCenter());
}

fn remove_request(center: &UNUserNotificationCenter) {
    let identifier = NSString::from_str(ID);
    let identifiers = NSArray::from_slice(&[identifier.as_ref()]);
    center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
    center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
}

pub fn pairing_request(peer: &str) {
    if cfg!(test) {
        let _ = peer;
        return;
    }
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1;
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("A system wants to pair with Daisy"));
    content.setBody(&NSString::from_str(&format!(
        "{peer} is waiting. Open Daisy to respond."
    )));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(ID), &content, None);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let authorized = center.clone();
    let callback = RcBlock::new(move |granted: objc2::runtime::Bool, error| {
        if !granted.as_bool() {
            tracing::warn!(?error, "pairing notification was not authorized");
        } else if GENERATION.load(std::sync::atomic::Ordering::Acquire) == generation {
            let completed_center = authorized.clone();
            let completed = RcBlock::new(move |error: *mut objc2_foundation::NSError| {
                if !error.is_null() {
                    tracing::warn!(?error, "pairing notification could not be delivered");
                }
                if GENERATION.load(std::sync::atomic::Ordering::Acquire) != generation {
                    remove_request(&completed_center);
                }
            });
            authorized.addNotificationRequest_withCompletionHandler(&request, Some(&completed));
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &callback);
}
