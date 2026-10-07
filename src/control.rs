//! Session ownership and layout decisions, independent of macOS.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use std::time::Instant;

use crate::identity::PublicKey;
use crate::input::Side;

pub const SETTLE: Duration = Duration::from_millis(150);

/// Tracks new physical input without treating heartbeats or ownership changes
/// as activity. Input seen while locked or following is never replayed later.
pub struct Activity {
    last: Option<Duration>,
}

impl Activity {
    pub fn new(last: Option<Duration>) -> Self {
        Self { last }
    }

    pub fn take(&mut self, physical: Option<Duration>, owns: bool, locked: bool) -> bool {
        let previous = std::mem::replace(&mut self.last, physical);
        physical.is_some() && physical != previous && owns && !locked
    }
}

/// What a person sees of one peer's link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Link {
    /// Authenticated wire protocol negotiated for this active peer.
    pub protocol: u16,
    /// Recent average round trip, in whole milliseconds.
    pub latency_ms: Option<u64>,
    /// Whether this system has control.
    pub in_control: bool,
    /// Whether this peer has control.
    pub peer_in_control: bool,
    /// Whether this peer's screen is locked.
    pub peer_locked: bool,
    /// Every round trip on the link so far.
    pub stats: crate::latency::Stats,
}

pub struct SharedControl {
    pub state: Mutex<Control>,
    pub interrupted: AtomicBool,
    pub diagnostics: crate::diagnostics::CaptureQueue,
    wake: tokio::sync::mpsc::Sender<()>,
    wakeups: Mutex<Option<tokio::sync::mpsc::Receiver<()>>>,
    activity: AtomicU64,
    started: Instant,
}

impl SharedControl {
    /// `me` is this system; `owner` has control at the start.
    pub fn new(me: PublicKey, owner: PublicKey) -> Self {
        let (wake, wakeups) = tokio::sync::mpsc::channel(1);
        Self {
            state: Mutex::new(Control::new(me, owner)),
            interrupted: AtomicBool::new(false),
            diagnostics: crate::diagnostics::CaptureQueue::default(),
            wake,
            wakeups: Mutex::new(Some(wakeups)),
            activity: AtomicU64::new(0),
            started: Instant::now(),
        }
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

    /// Last physical input, for sending activity only when it changes.
    pub fn physical_activity(&self) -> Option<Duration> {
        self.activity
            .load(Ordering::Acquire)
            .checked_sub(1)
            .map(Duration::from_nanos)
    }
}

/// Who has control across a group. Every claim names its claimant and a
/// generation, and every system keeps the greatest it has seen, ordered by
/// generation and then by key, so all members agree on one owner whatever
/// order claims arrive in.
#[derive(Debug)]
pub struct Control {
    me: PublicKey,
    owner: PublicKey,
    generation: u64,
    local_until: Duration,
    claimed_at: Option<Duration>,
}

impl Control {
    pub fn new(me: PublicKey, owner: PublicKey) -> Self {
        Self {
            me,
            owner,
            generation: 0,
            local_until: Duration::ZERO,
            claimed_at: None,
        }
    }

    /// Physical input immediately excludes remote injection. Claims are rate
    /// limited during simultaneous use.
    pub fn physical(&mut self, now: Duration) -> Option<u64> {
        self.local_until = now.saturating_add(SETTLE);
        if self.owns() || self.claimed_at.is_some_and(|last| now.saturating_sub(last) < SETTLE) {
            return None;
        }
        Some(self.take(now))
    }

    /// `by` claims control at `generation`. Returns whether the owner changed.
    pub fn claim(&mut self, generation: u64, by: PublicKey) -> bool {
        if (generation, by) > (self.generation, self.owner) {
            self.generation = generation;
            self.owner = by;
            true
        } else {
            false
        }
    }

    pub fn interrupt(&mut self, now: Duration) -> u64 {
        self.local_until = now.saturating_add(SETTLE);
        self.take(now)
    }

    fn take(&mut self, now: Duration) -> u64 {
        self.generation = self.generation.saturating_add(1);
        self.owner = self.me;
        self.claimed_at = Some(now);
        self.generation
    }

    /// Whether input `from` a peer, stamped `generation`, is played here.
    pub fn receives(&self, generation: u64, from: PublicKey, now: Duration) -> bool {
        self.owner == from && from != self.me && generation == self.generation && now >= self.local_until
    }

    pub fn receives_activity(
        &self,
        generation: u64,
        from: PublicKey,
        now: Duration,
        locked: bool,
        locally_busy: bool,
    ) -> bool {
        self.receives(generation, from, now) && !locked && !locally_busy
    }

    pub fn owns(&self) -> bool {
        self.owner == self.me
    }

    pub fn owner(&self) -> PublicKey {
        self.owner
    }

    /// This system.
    pub fn me(&self) -> PublicKey {
        self.me
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

    fn key(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    #[test]
    fn touching_a_follower_excludes_remote_input_immediately() {
        let (me, peer) = (key(1), key(2));
        let mut c = Control::new(me, me);
        assert!(c.claim(1, peer));
        assert!(c.receives(1, peer, Duration::from_secs(1)));
        assert_eq!(c.physical(Duration::from_secs(1)), Some(2));
        assert!(!c.receives(1, peer, Duration::from_secs(1)));
    }

    #[test]
    fn simultaneous_claims_converge_without_flapping() {
        let (a_key, b_key) = (key(2), key(1));
        let mut a = Control::new(a_key, a_key);
        let mut b = Control::new(b_key, a_key);
        let now = Duration::from_secs(1);
        assert!(b.claim(1, a_key) || b.owner() == a_key);
        a.claim(1, b_key);
        b.claim(1, b_key);
        assert_eq!(a.physical(now), Some(2));
        assert_eq!(b.physical(now), Some(2));
        // the greater key wins a tie
        assert!(!a.claim(2, b_key));
        assert!(b.claim(2, a_key));
        assert_eq!(b.physical(now + Duration::from_millis(1)), None);
        assert!(!b.receives(2, a_key, now + Duration::from_millis(2)));
        assert_eq!(b.physical(now + SETTLE), Some(3));
        assert!(a.claim(3, b_key));
        assert!(!a.claim(2, a_key));
    }

    #[test]
    fn three_systems_agree_on_one_owner_whatever_the_order() {
        let keys = [key(1), key(2), key(3)];
        let claims = [(4, keys[0]), (4, keys[2]), (3, keys[1]), (5, keys[1]), (5, keys[0])];
        let mut orders = vec![claims.to_vec()];
        orders.push(claims.iter().rev().copied().collect());
        orders.push(vec![claims[3], claims[0], claims[4], claims[2], claims[1]]);
        for order in orders {
            for me in keys {
                let mut c = Control::new(me, keys[0]);
                for (generation, by) in &order {
                    c.claim(*generation, *by);
                }
                assert_eq!((c.generation(), c.owner()), (5, keys[1]), "{order:?} on {me:?}");
            }
        }
    }

    #[test]
    fn only_the_owner_is_played() {
        let (me, owner, other) = (key(1), key(2), key(3));
        let mut c = Control::new(me, me);
        c.claim(4, owner);
        let now = Duration::from_secs(1);
        assert!(c.receives(4, owner, now));
        assert!(!c.receives(4, other, now), "a member that is not the owner");
        assert!(!c.receives(3, owner, now), "an older claim of the owner");
        assert!(!c.receives(4, me, now));
    }

    #[test]
    fn wakes_coalesce_without_waiting_for_the_session() {
        let control = SharedControl::new(key(1), key(1));
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

    #[test]
    fn activity_requires_new_physical_input() {
        let mut activity = Activity::new(None);
        assert!(!activity.take(None, true, false));
        let first = Some(Duration::from_secs(1));
        assert!(activity.take(first, true, false));
        assert!(!activity.take(first, true, false), "heartbeats cannot extend idle time");
        assert!(activity.take(Some(Duration::from_secs(2)), true, false));
    }

    #[test]
    fn activity_seen_while_locked_or_following_is_not_replayed() {
        let first = Some(Duration::from_secs(1));
        let mut activity = Activity::new(None);
        assert!(!activity.take(first, true, true));
        assert!(!activity.take(first, true, false), "unlocking is not new input");
        let second = Some(Duration::from_secs(2));
        assert!(!activity.take(second, false, false));
        assert!(!activity.take(second, true, false), "ownership alone is not new input");
        assert!(activity.take(Some(Duration::from_secs(3)), true, false));
        let mut existing = Activity::new(first);
        assert!(
            !existing.take(first, true, false),
            "starting a session is not new input"
        );
    }

    #[test]
    fn activity_is_accepted_only_from_the_unlocked_idle_current_owner() {
        let (me, owner, other) = (key(1), key(2), key(3));
        let mut control = Control::new(me, me);
        control.claim(4, owner);
        let now = Duration::from_secs(1);
        assert!(control.receives_activity(4, owner, now, false, false));
        assert!(!control.receives_activity(3, owner, now, false, false));
        assert!(!control.receives_activity(4, other, now, false, false));
        assert!(!control.receives_activity(4, me, now, false, false));
        assert!(!control.receives_activity(4, owner, now, true, false));
        assert!(!control.receives_activity(4, owner, now, false, true));
    }
}
