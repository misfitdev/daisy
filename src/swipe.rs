//! Multi-finger trackpad swipes: the ones that switch Spaces and open
//! Mission Control.
//!
//! macOS delivers these as private "DockControl" events, not public gesture
//! events. The driving system forwards every step of a swipe, so the peer
//! can replay it live and the swipe follows the fingers: paused halfway,
//! pulled back, or flicked. This module holds those steps, paces them for
//! replay, and recognizes a swipe's direction for systems that can only replay
//! it as a keyboard shortcut; `macos::swipe` reads and synthesizes the
//! events. The field meanings are pinned by hardware observations and tests.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// A swipe, named for the navigation it triggers rather than the direction
/// the fingers moved: `Right` goes to the next Space, `Left` to the previous
/// one, `Up` opens Mission Control and `Down` opens App Exposé or closes
/// Mission Control.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

impl SwipeDirection {
    /// Sign of a swipe's progress and velocity as macOS 27 reports them.
    /// Recognition and synthesis share it, so a captured swipe replays as
    /// the same navigation.
    ///
    /// Measured on hardware: real swipes to the left Space and up to
    /// Mission Control report negative progress. Replaying a swipe with the
    /// sign it was captured with cannot reveal a mix-up here, so this is
    /// pinned by tests with the recorded values.
    pub fn sign(self) -> f64 {
        match self {
            SwipeDirection::Right | SwipeDirection::Down => 1.0,
            SwipeDirection::Left | SwipeDirection::Up => -1.0,
        }
    }

    pub fn is_horizontal(self) -> bool {
        matches!(self, SwipeDirection::Left | SwipeDirection::Right)
    }
}

pub const MOTION_HORIZONTAL: i64 = 1;
pub const MOTION_VERTICAL: i64 = 2;

pub const PHASE_BEGAN: i64 = 1;
pub const PHASE_CHANGED: i64 = 2;
pub const PHASE_ENDED: i64 = 4;
pub const PHASE_CANCELLED: i64 = 8;

/// The fields of one DockControl event that recognition needs.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct DockEvent {
    pub motion: i64,
    pub phase: i64,
    pub progress: f64,
    pub velocity_x: f64,
    pub velocity_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwipeAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwipePhase {
    Began,
    Changed,
    Ended,
    Cancelled,
}

/// One step of a swipe as the trackpad reported it: `progress` in Spaces
/// travelled, and on the last step `velocity`, both with the sign real
/// swipes use (see `SwipeDirection::sign`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SwipeStep {
    pub axis: SwipeAxis,
    pub phase: SwipePhase,
    pub progress: f64,
    pub velocity: f64,
}

impl SwipeStep {
    pub fn from_dock(event: &DockEvent) -> Option<Self> {
        let (axis, velocity) = match event.motion {
            MOTION_HORIZONTAL => (SwipeAxis::Horizontal, event.velocity_x),
            MOTION_VERTICAL => (SwipeAxis::Vertical, event.velocity_y),
            _ => return None,
        };
        let phase = match event.phase {
            PHASE_BEGAN => SwipePhase::Began,
            PHASE_CHANGED => SwipePhase::Changed,
            PHASE_ENDED => SwipePhase::Ended,
            PHASE_CANCELLED => SwipePhase::Cancelled,
            _ => return None,
        };
        Some(Self {
            axis,
            phase,
            progress: event.progress,
            velocity,
        })
    }

    pub fn to_dock(&self) -> DockEvent {
        let (motion, velocity_x, velocity_y) = match self.axis {
            SwipeAxis::Horizontal => (MOTION_HORIZONTAL, self.velocity, 0.0),
            SwipeAxis::Vertical => (MOTION_VERTICAL, 0.0, self.velocity),
        };
        let phase = match self.phase {
            SwipePhase::Began => PHASE_BEGAN,
            SwipePhase::Changed => PHASE_CHANGED,
            SwipePhase::Ended => PHASE_ENDED,
            SwipePhase::Cancelled => PHASE_CANCELLED,
        };
        DockEvent {
            motion,
            phase,
            progress: self.progress,
            velocity_x,
            velocity_y,
        }
    }

    pub fn is_last(&self) -> bool {
        matches!(self.phase, SwipePhase::Ended | SwipePhase::Cancelled)
    }
}

/// The Dock drops swipe steps posted closer together than this, measured on
/// macOS 27: a swipe whose phases arrived back to back never moved.
pub const STEP_SPACING: Duration = Duration::from_millis(16);

/// Spaces out swipe steps for replay. Steps arrive as fast as the trackpad
/// reports them, and in bursts over the network; progress is absolute, so a
/// progress update still waiting when a newer one arrives is dropped rather
/// than let the replay fall behind the fingers. Beginnings and endings are
/// never dropped.
#[derive(Debug, Default)]
pub struct Pacer {
    pending: VecDeque<SwipeStep>,
    last_posted: Option<Instant>,
}

/// What a `Pacer` says to do next.
#[derive(Debug, PartialEq)]
pub enum Pace {
    Post(SwipeStep),
    Wait(Duration),
    Idle,
}

impl Pacer {
    pub fn push(&mut self, step: SwipeStep) {
        if step.phase == SwipePhase::Changed
            && let Some(waiting) = self.pending.back_mut()
            && waiting.phase == SwipePhase::Changed
        {
            *waiting = step;
            return;
        }
        self.pending.push_back(step);
    }

    pub fn next(&mut self, now: Instant) -> Pace {
        if self.pending.is_empty() {
            return Pace::Idle;
        }
        if let Some(last) = self.last_posted {
            let since = now.saturating_duration_since(last);
            if since < STEP_SPACING {
                return Pace::Wait(STEP_SPACING - since);
            }
        }
        self.last_posted = Some(now);
        Pace::Post(self.pending.pop_front().expect("checked not empty"))
    }
}

/// Recognizes completed swipes in a stream of DockControl events.
#[derive(Debug, Default)]
pub struct SwipeDetector {
    tracking: bool,
    fired: bool,
}

impl SwipeDetector {
    /// Returns the direction once per swipe, as soon as it is known: from
    /// the first movement, or from the ending velocity for a quick flick
    /// that reports none.
    pub fn feed(&mut self, event: &DockEvent) -> Option<SwipeDirection> {
        match event.phase {
            PHASE_BEGAN => {
                self.tracking = true;
                self.fired = false;
                None
            }
            _ if !self.tracking => None,
            PHASE_CHANGED if !self.fired => {
                let direction = direction(event.motion, event.progress);
                self.fired = direction.is_some();
                direction
            }
            PHASE_ENDED => {
                let direction = if self.fired {
                    None
                } else {
                    let velocity = if event.motion == MOTION_HORIZONTAL {
                        event.velocity_x
                    } else {
                        event.velocity_y
                    };
                    direction(event.motion, velocity)
                };
                self.reset();
                direction
            }
            PHASE_CANCELLED => {
                self.reset();
                None
            }
            _ => None,
        }
    }

    pub fn reset(&mut self) {
        self.tracking = false;
        self.fired = false;
    }
}

fn direction(motion: i64, value: f64) -> Option<SwipeDirection> {
    if value == 0.0 {
        return None;
    }
    [
        SwipeDirection::Left,
        SwipeDirection::Right,
        SwipeDirection::Up,
        SwipeDirection::Down,
    ]
    .into_iter()
    .find(|direction| {
        let axis = if direction.is_horizontal() {
            MOTION_HORIZONTAL
        } else {
            MOTION_VERTICAL
        };
        axis == motion && direction.sign() == value.signum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(motion: i64, phase: i64, progress: f64, velocity: f64) -> DockEvent {
        // real ended events report the same velocity on both axes
        DockEvent {
            motion,
            phase,
            progress,
            velocity_x: velocity,
            velocity_y: velocity,
        }
    }

    // Shaped like a swipe recorded from a real trackpad: began, changed
    // with growing progress, then ended with a velocity of the same sign.
    fn realistic(motion: i64, sign: f64) -> Vec<DockEvent> {
        let mut events = vec![event(motion, PHASE_BEGAN, sign * 0.0116, 0.0)];
        for progress in [0.0116, 0.1113, 0.3207, 0.6696, 0.9831] {
            events.push(event(motion, PHASE_CHANGED, sign * progress, 0.0));
        }
        events.push(event(motion, PHASE_ENDED, sign * 1.1168, sign * 5.7));
        events
    }

    fn feed_all(detector: &mut SwipeDetector, events: &[DockEvent]) -> Vec<SwipeDirection> {
        events.iter().filter_map(|event| detector.feed(event)).collect()
    }

    #[test]
    fn horizontal_swipes_fire_once() {
        let mut detector = SwipeDetector::default();
        assert_eq!(
            feed_all(&mut detector, &realistic(MOTION_HORIZONTAL, -1.0)),
            [SwipeDirection::Left]
        );
        assert_eq!(
            feed_all(&mut detector, &realistic(MOTION_HORIZONTAL, 1.0)),
            [SwipeDirection::Right]
        );
    }

    #[test]
    fn recorded_swipe_to_the_left_space_is_left() {
        // a real swipe recorded on macOS 27 (2026-09-25) that moved to the
        // Space on the left; replay tests cannot catch a flipped sign, this can
        let recorded = [
            event(MOTION_HORIZONTAL, PHASE_BEGAN, -0.0136, 0.0),
            event(MOTION_HORIZONTAL, PHASE_CHANGED, -0.0308, 0.0),
            event(MOTION_HORIZONTAL, PHASE_CHANGED, -0.0524, 0.0),
            event(MOTION_HORIZONTAL, PHASE_ENDED, -1.3401, -9.2),
        ];
        assert_eq!(
            feed_all(&mut SwipeDetector::default(), &recorded),
            [SwipeDirection::Left]
        );
    }

    #[test]
    fn recorded_swipe_up_is_up() {
        // a real swipe up to Mission Control recorded on macOS 27 (2026-09-22)
        let recorded = [
            event(MOTION_VERTICAL, PHASE_BEGAN, -0.0116, 0.0),
            event(MOTION_VERTICAL, PHASE_CHANGED, -0.0116, 0.0),
            event(MOTION_VERTICAL, PHASE_CHANGED, -0.0491, 0.0),
            event(MOTION_VERTICAL, PHASE_ENDED, -0.9072, -4.3),
        ];
        assert_eq!(feed_all(&mut SwipeDetector::default(), &recorded), [SwipeDirection::Up]);
    }

    #[test]
    fn vertical_swipes_fire_once() {
        let mut detector = SwipeDetector::default();
        assert_eq!(
            feed_all(&mut detector, &realistic(MOTION_VERTICAL, -1.0)),
            [SwipeDirection::Up]
        );
        assert_eq!(
            feed_all(&mut detector, &realistic(MOTION_VERTICAL, 1.0)),
            [SwipeDirection::Down]
        );
    }

    #[test]
    fn flick_without_progress_uses_the_ending_velocity() {
        let mut detector = SwipeDetector::default();
        let events = [
            event(MOTION_HORIZONTAL, PHASE_BEGAN, 0.0, 0.0),
            event(MOTION_HORIZONTAL, PHASE_ENDED, 0.0, -4.3),
        ];
        assert_eq!(feed_all(&mut detector, &events), [SwipeDirection::Left]);
    }

    #[test]
    fn waits_for_movement() {
        let mut detector = SwipeDetector::default();
        assert_eq!(detector.feed(&event(MOTION_VERTICAL, PHASE_BEGAN, 0.0, 0.0)), None);
        assert_eq!(detector.feed(&event(MOTION_VERTICAL, PHASE_CHANGED, 0.0, 0.0)), None);
        assert_eq!(
            detector.feed(&event(MOTION_VERTICAL, PHASE_CHANGED, -0.05, 0.0)),
            Some(SwipeDirection::Up)
        );
        assert_eq!(detector.feed(&event(MOTION_VERTICAL, PHASE_ENDED, 0.8, 5.2)), None);
    }

    #[test]
    fn cancelled_swipes_do_not_fire() {
        let mut detector = SwipeDetector::default();
        detector.feed(&event(MOTION_HORIZONTAL, PHASE_BEGAN, 0.0, 0.0));
        detector.feed(&event(MOTION_HORIZONTAL, PHASE_CANCELLED, 0.0, 0.0));
        assert_eq!(detector.feed(&event(MOTION_HORIZONTAL, PHASE_ENDED, 0.0, -5.0)), None);
    }

    #[test]
    fn a_swipe_already_under_way_is_ignored() {
        // began before tracking started, e.g. before the pointer crossed
        let mut detector = SwipeDetector::default();
        assert_eq!(detector.feed(&event(MOTION_HORIZONTAL, PHASE_CHANGED, -0.5, 0.0)), None);
        assert_eq!(detector.feed(&event(MOTION_HORIZONTAL, PHASE_ENDED, -1.0, -5.0)), None);
    }

    #[test]
    fn reset_forgets_a_swipe_in_progress() {
        let mut detector = SwipeDetector::default();
        detector.feed(&event(MOTION_HORIZONTAL, PHASE_BEGAN, 0.0, 0.0));
        detector.reset();
        assert_eq!(detector.feed(&event(MOTION_HORIZONTAL, PHASE_CHANGED, -0.5, 0.0)), None);
    }

    #[test]
    fn unknown_motion_never_fires() {
        let mut detector = SwipeDetector::default();
        detector.feed(&event(7, PHASE_BEGAN, 0.0, 0.0));
        assert_eq!(detector.feed(&event(7, PHASE_CHANGED, -0.5, 0.0)), None);
    }

    fn step(phase: SwipePhase, progress: f64) -> SwipeStep {
        SwipeStep {
            axis: SwipeAxis::Horizontal,
            phase,
            progress,
            velocity: 0.0,
        }
    }

    #[test]
    fn steps_round_trip_through_dock_events() {
        for event in realistic(MOTION_VERTICAL, -1.0) {
            let step = SwipeStep::from_dock(&event).unwrap();
            let back = step.to_dock();
            assert_eq!(
                (back.motion, back.phase, back.progress),
                (event.motion, event.phase, event.progress)
            );
            assert_eq!(back.velocity_y, event.velocity_y);
        }
        assert!(SwipeStep::from_dock(&event(7, PHASE_BEGAN, 0.0, 0.0)).is_none());
        assert!(SwipeStep::from_dock(&event(MOTION_HORIZONTAL, 3, 0.0, 0.0)).is_none());
    }

    #[test]
    fn a_swipe_replays_as_quickly_as_the_dock_allows() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        pacer.push(step(SwipePhase::Began, 0.0));
        pacer.push(step(SwipePhase::Changed, 0.1));
        assert_eq!(pacer.next(start), Pace::Post(step(SwipePhase::Began, 0.0)));
        assert_eq!(
            pacer.next(start + Duration::from_millis(5)),
            Pace::Wait(Duration::from_millis(11))
        );
        assert_eq!(
            pacer.next(start + STEP_SPACING),
            Pace::Post(step(SwipePhase::Changed, 0.1))
        );
        assert_eq!(pacer.next(start + STEP_SPACING * 2), Pace::Idle);
    }

    #[test]
    fn a_burst_of_progress_keeps_only_the_latest() {
        let mut pacer = Pacer::default();
        let start = Instant::now();
        pacer.push(step(SwipePhase::Began, 0.0));
        for progress in [0.1, 0.2, 0.3, 0.4] {
            pacer.push(step(SwipePhase::Changed, progress));
        }
        pacer.push(step(SwipePhase::Ended, 0.5));
        let mut posted = Vec::new();
        let mut now = start;
        loop {
            match pacer.next(now) {
                Pace::Post(step) => posted.push((step.phase, step.progress)),
                Pace::Wait(wait) => now += wait,
                Pace::Idle => break,
            }
        }
        assert_eq!(
            posted,
            [
                (SwipePhase::Began, 0.0),
                (SwipePhase::Changed, 0.4),
                (SwipePhase::Ended, 0.5)
            ]
        );
    }

    #[test]
    fn beginnings_and_endings_are_never_coalesced() {
        let mut pacer = Pacer::default();
        pacer.push(step(SwipePhase::Changed, 0.1));
        pacer.push(step(SwipePhase::Cancelled, 0.1));
        pacer.push(step(SwipePhase::Began, 0.0));
        pacer.push(step(SwipePhase::Changed, 0.2));
        assert_eq!(pacer.pending.len(), 4);
    }

    #[test]
    fn sign_and_recognition_agree() {
        for direction in [
            SwipeDirection::Left,
            SwipeDirection::Right,
            SwipeDirection::Up,
            SwipeDirection::Down,
        ] {
            let motion = if direction.is_horizontal() {
                MOTION_HORIZONTAL
            } else {
                MOTION_VERTICAL
            };
            assert_eq!(super::direction(motion, direction.sign()), Some(direction));
        }
    }
}
