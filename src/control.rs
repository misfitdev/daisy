//! Session ownership and layout decisions, independent of macOS.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;

use crate::input::Side;

pub const SETTLE: Duration = Duration::from_millis(150);

/// What a person sees of a running session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Link {
    /// Recent average round trip, in whole milliseconds.
    pub latency_ms: Option<u64>,
    /// Whether this system has control, rather than the peer.
    pub in_control: bool,
}

pub struct SharedControl {
    pub state: Mutex<Control>,
    pub interrupted: AtomicBool,
    wake: tokio::sync::mpsc::Sender<()>,
    wakeups: Mutex<Option<tokio::sync::mpsc::Receiver<()>>>,
    activity: AtomicU64,
    started: Instant,
    link: tokio::sync::watch::Sender<Link>,
}

impl SharedControl {
    pub fn new(initiator: bool) -> Self {
        let (wake, wakeups) = tokio::sync::mpsc::channel(1);
        Self {
            state: Mutex::new(Control::new(initiator)),
            interrupted: AtomicBool::new(false),
            wake,
            wakeups: Mutex::new(Some(wakeups)),
            activity: AtomicU64::new(0),
            started: Instant::now(),
            link: tokio::sync::watch::Sender::new(Link {
                latency_ms: None,
                in_control: initiator,
            }),
        }
    }

    /// Follows the session as a person sees it.
    pub fn watch_link(&self) -> tokio::sync::watch::Receiver<Link> {
        self.link.subscribe()
    }

    /// Records the latest latency and who has control.
    pub fn publish(&self, latency: Option<Duration>) {
        let in_control = self.state.lock().unwrap_or_else(|e| e.into_inner()).owns();
        let link = Link {
            latency_ms: latency.map(|latency| u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
            in_control,
        };
        // sent even when unchanged, so a watcher can refresh how long the session has run
        self.link.send_replace(link);
    }
    /// Wakes the session task after local input. Safe from the event tap: it
    /// never waits, and a wake already pending covers this one.
    pub fn wake(&self) {
        let _ = self.wake.try_send(());
    }

    /// The session task's end of `wake`. There is one, so only the first
    /// call gets it.
    pub fn take_wakeups(&self) -> Option<tokio::sync::mpsc::Receiver<()>> {
        self.wakeups.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn now(&self) -> Duration {
        self.started.elapsed()
    }
    pub fn note_physical(&self) {
        self.activity.store(
            self.now().as_nanos().min(u128::from(u64::MAX - 1)) as u64 + 1,
            Ordering::Release,
        );
    }
    pub fn local_busy(&self) -> bool {
        let at = self.activity.load(Ordering::Acquire);
        at != 0 && self.now().saturating_sub(Duration::from_nanos(at - 1)) < SETTLE
    }
}

#[derive(Debug)]
pub struct Control {
    initiator: bool,
    owner: bool,
    generation: u64,
    local_until: Duration,
    claimed_at: Option<Duration>,
}

impl Control {
    pub fn new(initiator: bool) -> Self {
        Self {
            initiator,
            owner: initiator,
            generation: 0,
            local_until: Duration::ZERO,
            claimed_at: None,
        }
    }

    /// Physical input immediately excludes remote injection. Claims are rate
    /// limited during simultaneous use; the initiating connection breaks ties.
    pub fn physical(&mut self, now: Duration) -> Option<u64> {
        self.local_until = now.saturating_add(SETTLE);
        if self.owner || self.claimed_at.is_some_and(|last| now.saturating_sub(last) < SETTLE) {
            return None;
        }
        self.generation = self.generation.saturating_add(1);
        self.owner = true;
        self.claimed_at = Some(now);
        Some(self.generation)
    }

    pub fn claim(&mut self, generation: u64) -> bool {
        if generation > self.generation || (generation == self.generation && !self.initiator && self.owner) {
            self.generation = generation;
            self.owner = false;
            true
        } else {
            false
        }
    }

    pub fn interrupt(&mut self, now: Duration) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.owner = true;
        self.claimed_at = Some(now);
        self.local_until = now.saturating_add(SETTLE);
        self.generation
    }

    pub fn receives(&self, generation: u64, now: Duration) -> bool {
        !self.owner && generation == self.generation && now >= self.local_until
    }

    pub fn owns(&self) -> bool {
        self.owner
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// The one relation both systems use, as seen from this one: the side chosen
/// most recently, with the initiator's winning a tie. `remote` is where the
/// peer places this system.
pub fn agreed_side(initiator: bool, local: (Side, u64), remote: (Side, u64)) -> (Side, u64) {
    if remote.1 > local.1 || (remote.1 == local.1 && !initiator) {
        (remote.0.opposite(), remote.1)
    } else {
        local
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touching_a_follower_excludes_remote_input_immediately() {
        let mut c = Control::new(false);
        assert!(c.claim(1));
        assert!(c.receives(1, Duration::from_secs(1)));
        assert_eq!(c.physical(Duration::from_secs(1)), Some(2));
        assert!(!c.receives(1, Duration::from_secs(1)));
    }

    #[test]
    fn simultaneous_claims_converge_without_flapping() {
        let mut a = Control::new(true);
        let mut b = Control::new(false);
        a.claim(1);
        b.claim(1);
        let now = Duration::from_secs(1);
        assert_eq!(a.physical(now), Some(2));
        assert_eq!(b.physical(now), Some(2));
        assert!(!a.claim(2));
        assert!(b.claim(2));
        assert_eq!(b.physical(now + Duration::from_millis(1)), None);
        assert!(!b.receives(2, now + Duration::from_millis(2)));
        assert_eq!(b.physical(now + SETTLE), Some(3));
        assert!(a.claim(3));
        assert!(!a.claim(2));
    }

    #[test]
    fn wakes_coalesce_without_waiting_for_the_session() {
        let control = SharedControl::new(false);
        let mut wakeups = control.take_wakeups().unwrap();
        for _ in 0..1000 {
            control.wake();
        }
        assert!(wakeups.try_recv().is_ok());
        assert!(wakeups.try_recv().is_err());
        control.wake();
        assert!(wakeups.try_recv().is_ok());
        assert!(control.take_wakeups().is_none());
    }

    #[test]
    fn layout_is_one_relation_in_both_directions() {
        for side in [Side::Left, Side::Right, Side::Above, Side::Below] {
            let initiator = agreed_side(true, (side, 5), (Side::Right, 5));
            let responder = agreed_side(false, (Side::Right, 5), (side, 5));
            assert_eq!(initiator, (side, 5));
            assert_eq!(responder, (side.opposite(), 5));
        }
    }

    #[test]
    fn the_latest_choice_wins_on_either_system() {
        let responder_chose_later = agreed_side(true, (Side::Right, 5), (Side::Above, 9));
        assert_eq!(responder_chose_later, (Side::Below, 9));
        let responder = agreed_side(false, (Side::Above, 9), (Side::Right, 5));
        assert_eq!(responder, (Side::Above, 9));
    }
}
