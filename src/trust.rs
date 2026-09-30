//! How long this system keeps trusting a peer it paired with.
//!
//! Trust is never forever unless asked for: a peer paired once, long ago,
//! must not be able to come back and take over the keyboard. Each side
//! applies its own policy to the other, and a session needs both to trust
//! each other, so the stricter of the two always wins.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Trust under the default policy ends after this long without a session.
pub const IDLE_LIMIT: Duration = Duration::from_secs(96 * 60 * 60);
/// How long a "just this once" peer may reconnect after an unexpected drop.
pub const ONCE_GRACE: Duration = Duration::from_secs(60);
/// The longest deadline `--trust <N>d` accepts, about ten years.
const MAX_DAYS: u32 = 3650;
const DAY: u64 = 24 * 60 * 60;

/// Seconds since the Unix epoch.
pub type Timestamp = u64;

pub fn now() -> Timestamp {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Policy {
    /// Until `IDLE_LIMIT` passes without a session; being connected renews it.
    #[default]
    Idle,
    /// Until this many days after pairing, however often the two connect.
    Days(u32),
    /// For one session, and a reconnect within `ONCE_GRACE` of it dropping.
    Once,
    /// Until forgotten.
    Forever,
}

impl Policy {
    /// When trust ends, or `None` if it never does. `live` means a session
    /// with the peer is running now, which keeps idle and once trust alive
    /// but not a deadline.
    pub fn expires_at(self, paired_at: Timestamp, last_seen: Timestamp, live: bool) -> Option<Timestamp> {
        match self {
            Policy::Forever => None,
            Policy::Days(days) => Some(paired_at.saturating_add(u64::from(days) * DAY)),
            _ if live => None,
            Policy::Idle => Some(last_seen.saturating_add(IDLE_LIMIT.as_secs())),
            Policy::Once => Some(last_seen.saturating_add(ONCE_GRACE.as_secs())),
        }
    }

    pub fn is_expired(self, paired_at: Timestamp, last_seen: Timestamp, live: bool, now: Timestamp) -> bool {
        self.expires_at(paired_at, last_seen, live)
            .is_some_and(|expires_at| now >= expires_at)
    }

    /// What a person reads in `daisy peers`.
    pub fn describe(self) -> String {
        match self {
            Policy::Idle => format!("until {} without a connection", span(IDLE_LIMIT.as_secs())),
            Policy::Days(days) => format!("for {days} day{} after pairing", if days == 1 { "" } else { "s" }),
            Policy::Once => "this session only".to_owned(),
            Policy::Forever => "forever".to_owned(),
        }
    }
}

impl fmt::Display for Policy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Policy::Idle => f.write_str("idle"),
            Policy::Days(days) => write!(f, "{days}d"),
            Policy::Once => f.write_str("once"),
            Policy::Forever => f.write_str("forever"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{0:?} is not a trust policy; use idle, <days>d such as 30d, once, or forever")]
pub struct UnknownPolicy(String);

impl FromStr for Policy {
    type Err = UnknownPolicy;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "idle" => Ok(Policy::Idle),
            "once" => Ok(Policy::Once),
            "forever" => Ok(Policy::Forever),
            _ => text
                .strip_suffix('d')
                .and_then(|days| days.parse().ok())
                .filter(|days| (1..=MAX_DAYS).contains(days))
                .map(Policy::Days)
                .ok_or_else(|| UnknownPolicy(text.to_owned())),
        }
    }
}

impl TryFrom<String> for Policy {
    type Error = UnknownPolicy;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        text.parse()
    }
}

impl From<Policy> for String {
    fn from(policy: Policy) -> Self {
        policy.to_string()
    }
}

/// A length of time to the precision a person cares about: "3d 4h", "5h 12m".
pub fn span(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / DAY, seconds % DAY / 3600, seconds % 3600 / 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "under a minute".to_owned(),
        (0, 0, _) => format!("{minutes}m"),
        (0, _, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAIRED: Timestamp = 1_000_000;
    const HOUR: u64 = 3600;

    #[test]
    fn idle_trust_ends_96_hours_after_the_last_session() {
        let seen = PAIRED + 30 * DAY;
        assert!(!Policy::Idle.is_expired(PAIRED, seen, false, seen + 96 * HOUR - 1));
        assert!(Policy::Idle.is_expired(PAIRED, seen, false, seen + 96 * HOUR));
    }

    #[test]
    fn a_live_session_keeps_idle_trust_but_not_a_deadline() {
        let long_after = PAIRED + 400 * DAY;
        assert!(!Policy::Idle.is_expired(PAIRED, PAIRED, true, long_after));
        assert!(Policy::Days(30).is_expired(PAIRED, long_after, true, PAIRED + 30 * DAY));
    }

    #[test]
    fn a_deadline_counts_from_pairing_not_from_the_last_session() {
        let seen = PAIRED + 29 * DAY;
        assert!(!Policy::Days(30).is_expired(PAIRED, seen, false, PAIRED + 30 * DAY - 1));
        assert!(Policy::Days(30).is_expired(PAIRED, seen, false, PAIRED + 30 * DAY));
    }

    #[test]
    fn once_trust_outlives_its_session_by_the_grace_window_only() {
        let dropped = PAIRED + HOUR;
        assert!(!Policy::Once.is_expired(PAIRED, PAIRED, true, dropped));
        assert!(!Policy::Once.is_expired(PAIRED, dropped, false, dropped + 59));
        assert!(Policy::Once.is_expired(PAIRED, dropped, false, dropped + 60));
    }

    #[test]
    fn forever_never_expires() {
        assert!(!Policy::Forever.is_expired(PAIRED, PAIRED, false, u64::MAX));
    }

    #[test]
    fn policies_round_trip_through_text() {
        for policy in [
            Policy::Idle,
            Policy::Days(1),
            Policy::Days(30),
            Policy::Once,
            Policy::Forever,
        ] {
            assert_eq!(policy.to_string().parse::<Policy>().unwrap(), policy);
        }
        for bad in ["", "0d", "3651d", "30", "d", "-1d", "always", "Idle"] {
            assert!(bad.parse::<Policy>().is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn spans_read_naturally() {
        assert_eq!(span(59), "under a minute");
        assert_eq!(span(42 * 60), "42m");
        assert_eq!(span(5 * HOUR + 12 * 60), "5h 12m");
        assert_eq!(span(3 * DAY + 4 * HOUR + 59), "3d 4h");
    }
}
