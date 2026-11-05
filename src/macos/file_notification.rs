//! Reviewable copied-file outcomes in macOS Notification Center.

use block2::RcBlock;
use objc2_foundation::NSString;
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

pub(super) fn show(id: &str, title: &str, body: &str) {
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(id), &content, None);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let authorized = center.clone();
    let callback = RcBlock::new(move |granted: objc2::runtime::Bool, error| {
        if granted.as_bool() {
            let completed = RcBlock::new(|error: *mut objc2_foundation::NSError| {
                if !error.is_null() {
                    tracing::warn!(?error, "copied-file notification could not be delivered");
                }
            });
            authorized.addNotificationRequest_withCompletionHandler(&request, Some(&completed));
        } else {
            tracing::warn!(?error, "copied-file notification was not authorized");
        }
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &callback);
}
