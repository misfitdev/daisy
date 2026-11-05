//! Pairing requests in macOS Notification Center.

use block2::RcBlock;
use objc2_foundation::NSString;
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

pub fn pairing_request(peer: &str) {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("A system wants to pair with Daisy"));
    content.setBody(&NSString::from_str(&format!(
        "{peer} is waiting. Open Daisy to respond."
    )));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
        &NSString::from_str("dev.misfit.daisy.pairing-request"),
        &content,
        None,
    );
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let authorized = center.clone();
    let callback = RcBlock::new(move |granted: objc2::runtime::Bool, error| {
        if granted.as_bool() {
            authorized.addNotificationRequest_withCompletionHandler(&request, None);
        } else {
            tracing::warn!(?error, "pairing notification was not authorized");
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &callback);
}
