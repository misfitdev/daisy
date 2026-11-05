//! Pairing requests in macOS Notification Center.

use block2::RcBlock;
use objc2_foundation::{NSArray, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest, UNUserNotificationCenter,
};

const ID_PREFIX: &str = "dev.misfit.daisy.pairing-request";

struct Requests {
    next: u64,
    active: Option<String>,
}

impl Requests {
    fn begin(&mut self) -> (String, Option<String>) {
        self.next += 1;
        let id = format!("{ID_PREFIX}-{}", self.next);
        let previous = self.active.replace(id.clone());
        (id, previous)
    }

    fn clear(&mut self) -> Option<String> {
        self.active.take()
    }

    fn is_active(&self, id: &str) -> bool {
        self.active.as_deref() == Some(id)
    }
}

static REQUESTS: std::sync::Mutex<Requests> = std::sync::Mutex::new(Requests { next: 0, active: None });

fn requests() -> std::sync::MutexGuard<'static, Requests> {
    REQUESTS.lock().unwrap_or_else(|error| error.into_inner())
}

pub fn clear() {
    let active = requests().clear();
    if cfg!(test) {
        return;
    }
    if let Some(id) = active {
        remove_request(&UNUserNotificationCenter::currentNotificationCenter(), &id);
    }
}

fn remove_request(center: &UNUserNotificationCenter, id: &str) {
    let identifier = NSString::from_str(id);
    let identifiers = NSArray::from_slice(&[identifier.as_ref()]);
    center.removePendingNotificationRequestsWithIdentifiers(&identifiers);
    center.removeDeliveredNotificationsWithIdentifiers(&identifiers);
}

pub fn pairing_request(peer: &str) {
    if cfg!(test) {
        let _ = peer;
        return;
    }
    let (id, previous) = requests().begin();
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str("A system wants to pair with Daisy"));
    content.setBody(&NSString::from_str(&format!(
        "{peer} is waiting. Open Daisy to respond."
    )));
    let request =
        UNNotificationRequest::requestWithIdentifier_content_trigger(&NSString::from_str(&id), &content, None);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    if let Some(previous) = previous {
        remove_request(&center, &previous);
    }
    let authorized = center.clone();
    let callback = RcBlock::new(
        move |granted: objc2::runtime::Bool, error: *mut objc2_foundation::NSError| {
            if !granted.as_bool() {
                // SAFETY: Foundation passes null or a valid NSError for this callback.
                let error = unsafe { error.as_ref() };
                tracing::warn!(?error, "pairing notification was not authorized");
            } else if requests().is_active(&id) {
                let completed_center = authorized.clone();
                let completed_id = id.clone();
                let completed = RcBlock::new(move |error: *mut objc2_foundation::NSError| {
                    // SAFETY: Foundation passes null or a valid NSError for this callback.
                    if let Some(error) = unsafe { error.as_ref() } {
                        tracing::warn!(?error, "pairing notification could not be delivered");
                    }
                    if !requests().is_active(&completed_id) {
                        remove_request(&completed_center, &completed_id);
                    }
                });
                authorized.addNotificationRequest_withCompletionHandler(&request, Some(&completed));
            }
        },
    );
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert, &callback);
}

#[cfg(test)]
mod tests {
    #[test]
    fn stale_request_cleanup_cannot_remove_the_new_request() {
        let mut requests = super::Requests { next: 0, active: None };
        let (first, previous) = requests.begin();
        assert!(previous.is_none());
        let (second, previous) = requests.begin();
        assert_eq!(previous.as_deref(), Some(first.as_str()));
        assert_ne!(first, second);
        assert!(!requests.is_active(&first));
        assert!(requests.is_active(&second));
        assert_eq!(requests.clear().as_deref(), Some(second.as_str()));
        assert!(!requests.is_active(&second));
    }
}
