//! Round-trip latency, measured from the session heartbeat.
//!
//! Times are offsets from any fixed start, so tests can supply their own.

use std::collections::VecDeque;
use std::time::Duration;

/// How many recent round trips the average covers.
pub const WINDOW: usize = 5;
/// A round trip slower than this is logged.
pub const SPIKE: Duration = Duration::from_millis(50);

/// Pings still waiting for their pong, and recent round trips.
#[derive(Debug, Default)]
pub struct Meter {
    waiting: VecDeque<(u64, Duration)>,
    recent: VecDeque<Duration>,
}

impl Meter {
    /// A ping with `nonce` went out at `at`.
    pub fn sent(&mut self, nonce: u64, at: Duration) {
        // a peer that stops answering must not grow this without bound
        if self.waiting.len() >= WINDOW * 2 {
            self.waiting.pop_front();
        }
        self.waiting.push_back((nonce, at));
    }

    /// A pong for `nonce` arrived at `at`. Returns its round trip, or `None`
    /// if it answers no ping still waiting. Earlier pings still waiting were
    /// lost and are dropped.
    pub fn answered(&mut self, nonce: u64, at: Duration) -> Option<Duration> {
        let index = self.waiting.iter().position(|(waiting, _)| *waiting == nonce)?;
        let (_, sent) = self.waiting.drain(..=index).next_back()?;
        let round_trip = at.saturating_sub(sent);
        if self.recent.len() == WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back(round_trip);
        Some(round_trip)
    }

    /// The average of the recent round trips, once there is one.
    pub fn average(&self) -> Option<Duration> {
        let count = u32::try_from(self.recent.len()).ok().filter(|count| *count > 0)?;
        Some(self.recent.iter().sum::<Duration>() / count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn nothing_is_known_before_the_first_pong() {
        let mut meter = Meter::default();
        meter.sent(1, ms(0));
        assert_eq!(meter.average(), None);
    }

    #[test]
    fn a_pong_measures_its_own_ping() {
        let mut meter = Meter::default();
        meter.sent(1, ms(1000));
        meter.sent(2, ms(2000));
        assert_eq!(meter.answered(2, ms(2004)), Some(ms(4)));
        assert_eq!(meter.average(), Some(ms(4)));
    }

    #[test]
    fn the_average_covers_only_the_recent_window() {
        let mut meter = Meter::default();
        let mut now = ms(0);
        for (nonce, round_trip) in [(1, 100), (2, 2), (3, 4), (4, 2), (5, 4), (6, 3)] {
            meter.sent(nonce, now);
            assert_eq!(meter.answered(nonce, now + ms(round_trip)), Some(ms(round_trip)));
            now += ms(1000);
        }
        assert_eq!(meter.average(), Some(ms(3)));
    }

    #[test]
    fn stray_and_repeated_pongs_are_ignored() {
        let mut meter = Meter::default();
        meter.sent(1, ms(0));
        meter.sent(2, ms(1000));
        assert_eq!(meter.answered(9, ms(1001)), None);
        assert_eq!(meter.answered(2, ms(1010)), Some(ms(10)));
        // ping 1 was lost; its late pong must not count as a 2 second trip
        assert_eq!(meter.answered(1, ms(2000)), None);
        assert_eq!(meter.answered(2, ms(2000)), None);
        assert_eq!(meter.average(), Some(ms(10)));
    }

    #[test]
    fn unanswered_pings_are_bounded() {
        let mut meter = Meter::default();
        for nonce in 0..1000 {
            meter.sent(nonce, ms(nonce * 1000));
        }
        assert!(meter.waiting.len() <= WINDOW * 2);
        assert_eq!(meter.answered(999, ms(999_005)), Some(ms(5)));
    }
}
