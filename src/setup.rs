//! The walkthrough that gets both macOS permissions granted.
//!
//! macOS only lets a person grant Accessibility and Input Monitoring in
//! System Settings, so Daisy asks for one at a time, opens the right pane,
//! and moves on as each is switched on.

use crate::permissions::Access;

/// The display sets a person checked, at one shared arrangement version.
/// Offsets travel in Arrangement; this record distinguishes confirmation
/// from automatic placement and display-overlap repair.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Confirmation<K> {
    pub version: u64,
    pub author: K,
    pub members: Vec<(K, Vec<crate::input::Rect>)>,
}

impl<K: Copy + Ord> Confirmation<K> {
    pub fn new(group: &crate::layout::Group<K>, (version, author): (u64, K)) -> Self {
        let mut members: Vec<_> = group
            .members
            .iter()
            .map(|m| {
                let mut displays = m.displays.clone();
                displays.sort_by(|a, b| {
                    a.x.total_cmp(&b.x)
                        .then(a.y.total_cmp(&b.y))
                        .then(a.width.total_cmp(&b.width))
                        .then(a.height.total_cmp(&b.height))
                });
                (m.key, displays)
            })
            .collect();
        members.sort_by_key(|(key, _)| *key);
        Self {
            version,
            author,
            members,
        }
    }

    /// Reconnecting unchanged members, including a subset of the checked
    /// group, needs no new question. New or changed displays do.
    pub fn covers(&self, current: &Self) -> bool {
        current.members.iter().all(|member| self.members.contains(member))
    }
}

impl Confirmation<crate::identity::PublicKey> {
    pub fn fingerprint(&self) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        hash.update(b"daisy-arrangement-confirmation-v1");
        for (key, displays) in &self.members {
            hash.update(key.as_bytes());
            hash.update((displays.len() as u64).to_le_bytes());
            for display in displays {
                for value in [display.x, display.y, display.width, display.height] {
                    // Treat negative zero as the same coordinate.
                    hash.update(if value == 0.0 { 0.0f64 } else { value }.to_le_bytes());
                }
            }
        }
        hash.finalize().into()
    }
}

pub fn check_arrangement<K: Copy + Ord>(
    group: &crate::layout::Group<K>,
    me: K,
    version: (u64, K),
    confirmed: Option<&Confirmation<K>>,
) -> bool {
    if group.members.len() < 2 {
        return false;
    }
    let current = Confirmation::new(group, version);
    !confirmed.is_some_and(|saved| {
        saved.covers(&current) && (group.unreachable(me).is_empty() || (saved.version, saved.author) == version)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Accessibility,
    InputMonitoring,
    /// Both are granted, but macOS applies Input Monitoring to Daisy only
    /// once it reopens.
    Reopen,
    Done,
}

/// The step to show. `reads_input` is whether macOS lets this process read
/// input right now, which can lag Input Monitoring reporting granted.
pub fn step(accessibility: Access, input_monitoring: Access, reads_input: bool) -> Step {
    match (accessibility, input_monitoring) {
        (Access::Granted, Access::Granted) if reads_input => Step::Done,
        (Access::Granted, Access::Granted) => Step::Reopen,
        (Access::Granted, _) => Step::InputMonitoring,
        _ => Step::Accessibility,
    }
}

/// How one permission's row looks at `step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Allowed,
    /// The one to allow now.
    Current,
    /// Waits for the one before it.
    Later,
}

pub fn row(step: Step, permission: Step) -> Row {
    let order = |step| match step {
        Step::Accessibility => 0,
        Step::InputMonitoring => 1,
        Step::Reopen | Step::Done => 2,
    };
    match order(permission).cmp(&order(step)) {
        std::cmp::Ordering::Less => Row::Allowed,
        std::cmp::Ordering::Equal => Row::Current,
        std::cmp::Ordering::Greater => Row::Later,
    }
}

/// Every native sharing session discovers and listens, including address connections.
pub fn can_add_system(active: bool, always: bool, adding: bool) -> bool {
    active && !always && !adding
}

/// Address-dialog submission preserves arrangement and trust, and clears any
/// previously selected peer key because the address names a new attempt.
pub fn connect_by_address(
    current: &crate::controller::SessionSettings,
    address: &str,
) -> Option<crate::controller::SessionSettings> {
    let address = address.trim();
    if address.is_empty() {
        return None;
    }
    let mut settings = current.clone();
    settings.connection = crate::controller::Connection::Connect {
        address: address.to_owned(),
        peer: None,
    };
    Some(settings)
}

#[cfg(test)]
mod tests {
    #[test]
    fn address_sessions_can_add_another_system() {
        assert!(can_add_system(true, false, false));
        assert!(!can_add_system(false, false, false));
        assert!(!can_add_system(true, true, false));
        assert!(!can_add_system(true, false, true));
    }

    #[test]
    fn address_submission_trims_and_preserves_session_choices() {
        use crate::controller::{Connection, SessionSettings};
        let current = SessionSettings {
            connection: Connection::Connect {
                address: "old.local".to_owned(),
                peer: Some("old-key".to_owned()),
            },
            side: crate::input::Side::Left,
            trust: crate::trust::Policy::Days(30),
        };
        let expected = SessionSettings {
            connection: Connection::Connect {
                address: "192.168.1.20:24850".to_owned(),
                peer: None,
            },
            side: current.side,
            trust: current.trust,
        };
        assert_eq!(
            super::connect_by_address(&current, " 192.168.1.20:24850 "),
            Some(expected)
        );
        assert_eq!(super::connect_by_address(&current, "   "), None);
        assert_eq!(super::connect_by_address(&current, ""), None);
    }

    use super::*;
    use Access::*;

    #[test]
    fn screen_confirmation_survives_reconnects_but_checks_changed_displays_and_stranded_members() {
        use crate::layout::{Group, Member};
        let screen = crate::input::Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 100.0,
        };
        let mut group = Group {
            members: vec![
                Member {
                    key: 0u8,
                    displays: vec![screen],
                    offset: (0.0, 0.0),
                },
                Member {
                    key: 1,
                    displays: vec![screen],
                    offset: (100.0, 0.0),
                },
            ],
        };
        assert!(check_arrangement(&group, 0, (1, 0), None), "first join");
        let checked = Confirmation::new(&group, (1, 0));
        assert!(!check_arrangement(&group, 0, (1, 0), Some(&checked)));
        let disconnected = Group {
            members: vec![group.members[0].clone()],
        };
        assert!(!check_arrangement(&disconnected, 0, (1, 0), Some(&checked)));
        assert!(
            !check_arrangement(&group, 0, (1, 0), Some(&checked)),
            "unchanged reconnect"
        );
        group.members[1].offset = (500.0, 0.0);
        assert!(
            check_arrangement(&group, 0, (2, 0), Some(&checked)),
            "stranded after moving"
        );
        let stranded_checked = Confirmation::new(&group, (2, 0));
        assert!(
            !check_arrangement(&group, 0, (2, 0), Some(&stranded_checked)),
            "Looks Right is explicit"
        );
        group.members[1].displays[0].width = 120.0;
        assert!(
            check_arrangement(&group, 0, (2, 0), Some(&stranded_checked)),
            "display change"
        );
        group.members[1].displays[0].width = 100.0;
        group.members.push(Member {
            key: 2,
            displays: vec![screen],
            offset: (600.0, 0.0),
        });
        assert!(
            check_arrangement(&group, 0, (2, 0), Some(&stranded_checked)),
            "new member"
        );
        let full = Confirmation::new(&group, (2, 0));
        group.members.remove(2);
        assert!(!check_arrangement(&group, 0, (2, 0), Some(&full)), "unchanged subset");
    }

    #[test]
    fn accessibility_comes_first() {
        assert_eq!(step(Denied, Undetermined, false), Step::Accessibility);
        assert_eq!(step(Denied, Granted, true), Step::Accessibility);
    }

    #[test]
    fn input_monitoring_follows_accessibility() {
        assert_eq!(step(Granted, Undetermined, false), Step::InputMonitoring);
        assert_eq!(step(Granted, Denied, false), Step::InputMonitoring);
    }

    #[test]
    fn granted_input_monitoring_that_does_not_apply_yet_asks_to_reopen() {
        assert_eq!(step(Granted, Granted, false), Step::Reopen);
        assert_eq!(step(Granted, Granted, true), Step::Done);
    }

    #[test]
    fn rows_show_what_is_allowed_and_what_is_next() {
        use Step::*;
        assert_eq!(row(Accessibility, Accessibility), Row::Current);
        assert_eq!(row(Accessibility, InputMonitoring), Row::Later);
        assert_eq!(row(InputMonitoring, Accessibility), Row::Allowed);
        assert_eq!(row(InputMonitoring, InputMonitoring), Row::Current);
        for done in [Reopen, Done] {
            assert_eq!(row(done, Accessibility), Row::Allowed);
            assert_eq!(row(done, InputMonitoring), Row::Allowed);
        }
    }
}
