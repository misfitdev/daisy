//! Recognizing a shake of the pointer, so the following system can
//! enlarge it the way macOS does for its own mouse.
//!
//! macOS detects the shake from real hardware input only; motion posted as
//! events never triggers it, so the following system watches the motion it
//! replays and magnifies the pointer itself.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// A stroke must cover this many points to count toward a shake.
const MIN_STROKE: f64 = 80.0;
/// A stroke slower than this is steering, not shaking.
const MAX_STROKE_TIME: Duration = Duration::from_millis(200);
/// Direction reversals needed, within `WINDOW`, to call it a shake.
const REVERSALS: usize = 5;
const WINDOW: Duration = Duration::from_secs(1);

/// How large a shaken pointer grows, relative to its usual size.
pub const MAX_SCALE: f64 = 4.0;
/// Growth and shrinking speeds, in multiples of the usual size per second.
const GROW_RATE: f64 = 12.0;
const SHRINK_RATE: f64 = 20.0;
/// How long the pointer stays large after the last sign of shaking.
pub const HOLD: Duration = Duration::from_millis(150);

#[derive(Debug, Default)]
struct Axis {
    direction: i8,
    travel: f64,
    started: Option<Instant>,
}

impl Axis {
    /// Returns whether this motion ended a stroke that counts toward a shake.
    fn feed(&mut self, delta: f64, now: Instant) -> bool {
        let direction = if delta > 0.0 {
            1
        } else if delta < 0.0 {
            -1
        } else {
            return false;
        };
        if direction == self.direction {
            self.travel += delta.abs();
            return false;
        }
        let counts = self.direction != 0
            && self.travel >= MIN_STROKE
            && self.started.is_some_and(|started| now - started <= MAX_STROKE_TIME);
        *self = Self {
            direction,
            travel: delta.abs(),
            started: Some(now),
        };
        counts
    }
}

/// Watches pointer motion for a shake, horizontal or vertical.
#[derive(Debug, Default)]
pub struct ShakeDetector {
    horizontal: Axis,
    vertical: Axis,
    reversals: VecDeque<Instant>,
}

impl ShakeDetector {
    /// Feed one motion; returns true when it reverses a shake, so a
    /// continuing shake reports several times a second and a finished one
    /// stops reporting at once.
    pub fn feed(&mut self, dx: f64, dy: f64, now: Instant) -> bool {
        let mut reversed = false;
        for axis_reversed in [self.horizontal.feed(dx, now), self.vertical.feed(dy, now)] {
            if axis_reversed {
                self.reversals.push_back(now);
                reversed = true;
            }
        }
        while self.reversals.front().is_some_and(|&at| now - at > WINDOW) {
            self.reversals.pop_front();
        }
        reversed && self.reversals.len() >= REVERSALS
    }
}

/// The pointer's size over time: growing while shaken, then easing back.
#[derive(Debug)]
pub struct Zoom {
    usual: f64,
    scale: f64,
}

impl Zoom {
    pub fn new(usual: f64) -> Self {
        Self { usual, scale: usual }
    }

    /// Advance by `elapsed`; returns the scale to show now.
    pub fn step(&mut self, shaking: bool, elapsed: Duration) -> f64 {
        let seconds = elapsed.as_secs_f64();
        let largest = self.usual * MAX_SCALE;
        self.scale = if shaking {
            (self.scale + GROW_RATE * self.usual * seconds).min(largest)
        } else {
            (self.scale - SHRINK_RATE * self.usual * seconds).max(self.usual)
        };
        self.scale
    }

    pub fn at_rest(&self) -> bool {
        self.scale <= self.usual
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(8);

    /// Motion back and forth: `strokes` strokes of `length` points, each
    /// made of 8 ms frames, `frames` per stroke.
    fn wiggle(detector: &mut ShakeDetector, start: Instant, strokes: u32, length: f64, frames: u32) -> (bool, Instant) {
        let mut now = start;
        let mut shaken = false;
        for stroke in 0..strokes {
            let sign = if stroke % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..frames {
                now += FRAME;
                shaken |= detector.feed(sign * length / f64::from(frames), 0.0, now);
            }
        }
        (shaken, now)
    }

    #[test]
    fn a_quick_wiggle_is_a_shake() {
        let (shaken, _) = wiggle(&mut ShakeDetector::default(), Instant::now(), 6, 120.0, 10);
        assert!(shaken);
    }

    #[test]
    fn a_vertical_wiggle_is_a_shake() {
        let mut detector = ShakeDetector::default();
        let mut now = Instant::now();
        let mut shaken = false;
        for stroke in 0..6 {
            let sign = if stroke % 2 == 0 { 1.0 } else { -1.0 };
            for _ in 0..10 {
                now += FRAME;
                shaken |= detector.feed(0.0, sign * 12.0, now);
            }
        }
        assert!(shaken);
    }

    #[test]
    fn motion_after_a_shake_is_not_reported_as_shaking() {
        let mut detector = ShakeDetector::default();
        let (shaken, mut now) = wiggle(&mut detector, Instant::now(), 6, 120.0, 10);
        assert!(shaken);
        // carrying on steadily the way the last stroke went
        for _ in 0..50 {
            now += FRAME;
            assert!(!detector.feed(-5.0, 0.0, now));
        }
    }

    #[test]
    fn small_corrections_are_not_a_shake() {
        // lining up on a target: many reversals, but only a few points each
        let (shaken, _) = wiggle(&mut ShakeDetector::default(), Instant::now(), 20, 6.0, 3);
        assert!(!shaken);
    }

    #[test]
    fn slow_sweeps_are_not_a_shake() {
        // long, deliberate strokes of about a quarter second each
        let (shaken, _) = wiggle(&mut ShakeDetector::default(), Instant::now(), 8, 400.0, 30);
        assert!(!shaken);
    }

    #[test]
    fn four_reversals_are_not_enough() {
        let (shaken, _) = wiggle(&mut ShakeDetector::default(), Instant::now(), 5, 120.0, 10);
        assert!(!shaken);
    }

    #[test]
    fn a_brisk_but_short_wiggle_is_not_a_shake() {
        // reported on hardware as setting it off by accident: strokes of
        // about 60 points, 80 ms each
        let (shaken, _) = wiggle(&mut ShakeDetector::default(), Instant::now(), 10, 60.0, 10);
        assert!(!shaken);
    }

    #[test]
    fn the_pointer_is_back_to_size_soon_after_shaking_stops() {
        // reported on hardware as snapping back too slowly
        let settle = HOLD.as_secs_f64() + (MAX_SCALE - 1.0) / SHRINK_RATE;
        assert!(settle <= 0.35, "takes {settle:.2} s");
    }

    #[test]
    fn reversals_spread_over_time_do_not_add_up() {
        let mut detector = ShakeDetector::default();
        let mut now = Instant::now();
        for _ in 0..4 {
            let (shaken, end) = wiggle(&mut detector, now, 2, 120.0, 10);
            assert!(!shaken);
            now = end + Duration::from_secs(2);
        }
    }

    #[test]
    fn zoom_grows_to_the_limit_then_eases_back() {
        let mut zoom = Zoom::new(1.0);
        let mut scale = 1.0;
        for _ in 0..60 {
            scale = zoom.step(true, Duration::from_millis(16));
        }
        assert_eq!(scale, MAX_SCALE);
        assert!(!zoom.at_rest());

        for _ in 0..60 {
            scale = zoom.step(false, Duration::from_millis(16));
        }
        assert_eq!(scale, 1.0);
        assert!(zoom.at_rest());
    }

    #[test]
    fn zoom_is_relative_to_the_chosen_pointer_size() {
        let mut zoom = Zoom::new(2.0);
        for _ in 0..60 {
            zoom.step(true, Duration::from_millis(16));
        }
        assert_eq!(zoom.step(true, Duration::ZERO), 2.0 * MAX_SCALE);
        for _ in 0..60 {
            zoom.step(false, Duration::from_millis(16));
        }
        assert_eq!(zoom.step(false, Duration::ZERO), 2.0);
    }
}
