//! Round-trip latency, measured from the session heartbeat.
//!
//! Times are offsets from any fixed start, so tests can supply their own.

use std::collections::VecDeque;
use std::time::Duration;

/// How many recent round trips the average covers.
pub const WINDOW: usize = 5;
/// A round trip slower than this is logged.
pub const SPIKE: Duration = Duration::from_millis(50);

/// Pings still waiting for their pong, recent round trips, and every round
/// trip so far by size.
#[derive(Debug, Default)]
pub struct Meter {
    waiting: VecDeque<(u64, Duration)>,
    recent: VecDeque<Duration>,
    histogram: Histogram,
}

/// Round trips on one link so far, in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Stats {
    pub count: u64,
    pub p50_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// The file a running Daisy keeps its links' round trips in, for `daisy stats`.
pub const STATS_FILE: &str = "stats.toml";
/// Round trips in the file older than this are from a link that has ended.
pub const STATS_FRESH: u64 = 5;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LinkStats {
    pub peer: String,
    /// When these were measured, in Unix seconds.
    pub updated: u64,
    #[serde(flatten)]
    pub stats: Stats,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct StatsFile {
    #[serde(default, rename = "link")]
    links: Vec<LinkStats>,
}

/// Writes every link's round trips.
pub fn save(home: &std::path::Path, links: &[LinkStats]) -> anyhow::Result<()> {
    let text = toml::to_string(&StatsFile { links: links.to_vec() })?;
    let path = home.join(STATS_FILE);
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

/// The links measured within `STATS_FRESH` of `now`.
pub fn load(home: &std::path::Path, now: u64) -> Vec<LinkStats> {
    let Ok(text) = std::fs::read_to_string(home.join(STATS_FILE)) else {
        return Vec::new();
    };
    let file: StatsFile = toml::from_str(&text).unwrap_or_default();
    file.links
        .into_iter()
        .filter(|link| now.saturating_sub(link.updated) <= STATS_FRESH)
        .collect()
}

/// Each power of two is split this many ways, so a percentile is off by at
/// most an eighth.
const STEPS: u32 = 8;
/// Up to 2^27 µs, over two minutes.
const BUCKETS: usize = 28 * STEPS as usize;

/// Counts of round trips by size, on a log scale.
#[derive(Debug, Clone)]
struct Histogram {
    counts: Box<[u64; BUCKETS]>,
    count: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: Box::new([0; BUCKETS]),
            count: 0,
            max: 0,
        }
    }
}

impl Histogram {
    fn record(&mut self, micros: u64) {
        self.counts[bucket(micros)] += 1;
        self.count += 1;
        self.max = self.max.max(micros);
    }

    /// The smallest bucket bound that `share` of the round trips fit under.
    fn percentile(&self, share: f64) -> u64 {
        let wanted = ((self.count as f64) * share).ceil().max(1.0) as u64;
        let mut seen = 0;
        for (index, count) in self.counts.iter().enumerate() {
            seen += count;
            if seen >= wanted {
                return upper(index).min(self.max);
            }
        }
        self.max
    }
}

fn bucket(micros: u64) -> usize {
    let micros = micros.max(1);
    let power = 63 - micros.leading_zeros();
    let step = if power >= 3 {
        ((micros >> (power - 3)) & u64::from(STEPS - 1)) as u32
    } else {
        ((micros << (3 - power)) & u64::from(STEPS - 1)) as u32
    };
    ((power * STEPS + step) as usize).min(BUCKETS - 1)
}

/// The largest value in `index`'s bucket.
fn upper(index: usize) -> u64 {
    let (power, step) = (index as u32 / STEPS, index as u32 % STEPS);
    let low = (1u64 << power) + ((u64::from(step) << power) >> 3);
    let width = ((1u64 << power) >> 3).max(1);
    low + width - 1
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
        self.histogram
            .record(u64::try_from(round_trip.as_micros()).unwrap_or(u64::MAX));
        Some(round_trip)
    }

    /// Every round trip so far, by percentile.
    pub fn stats(&self) -> Stats {
        if self.histogram.count == 0 {
            return Stats::default();
        }
        Stats {
            count: self.histogram.count,
            p50_us: self.histogram.percentile(0.5),
            p99_us: self.histogram.percentile(0.99),
            max_us: self.histogram.max,
        }
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
    fn percentiles_are_within_an_eighth_of_the_truth() {
        let mut meter = Meter::default();
        let mut now = Duration::ZERO;
        // 1..=1000 ms in order
        for nonce in 1..=1000u64 {
            meter.sent(nonce, now);
            meter.answered(nonce, now + ms(nonce));
            now += ms(2000);
        }
        let stats = meter.stats();
        assert_eq!(stats.count, 1000);
        assert_eq!(stats.max_us, 1_000_000);
        for (got, truth) in [(stats.p50_us, 500_000.0), (stats.p99_us, 990_000.0)] {
            let error = (got as f64 - truth).abs() / truth;
            assert!(error <= 0.125, "{got} for {truth}");
            assert!(got as f64 >= truth, "{got} is under {truth}");
        }
    }

    #[test]
    fn a_single_round_trip_is_every_percentile() {
        let mut meter = Meter::default();
        assert_eq!(meter.stats(), Stats::default());
        meter.sent(1, ms(0));
        meter.answered(1, Duration::from_micros(1234));
        let stats = meter.stats();
        assert_eq!(
            (stats.count, stats.p50_us, stats.p99_us, stats.max_us),
            (1, 1234, 1234, 1234)
        );
    }

    #[test]
    fn buckets_cover_every_size_in_order() {
        let mut last = 0;
        for micros in [1, 2, 3, 7, 8, 9, 15, 16, 100, 1_000, 65_535, 1 << 26, u64::MAX] {
            let index = bucket(micros);
            assert!(index >= last, "{micros}");
            assert!(
                micros > (1 << 27) || upper(index) >= micros,
                "{micros} above its bucket"
            );
            last = index;
        }
    }

    #[test]
    fn stats_are_read_back_only_while_fresh() {
        let home = tempfile::tempdir().unwrap();
        assert!(load(home.path(), 100).is_empty());
        let link = LinkStats {
            peer: "Studio".to_owned(),
            updated: 100,
            stats: Stats {
                count: 3,
                p50_us: 900,
                p99_us: 4000,
                max_us: 4100,
            },
        };
        save(home.path(), std::slice::from_ref(&link)).unwrap();
        assert_eq!(load(home.path(), 100 + STATS_FRESH), [link]);
        assert!(load(home.path(), 101 + STATS_FRESH).is_empty());
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
