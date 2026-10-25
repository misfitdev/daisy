//! Where every display of every system in a group sits, and where the
//! pointer goes when it leaves one system's displays.
//!
//! Each system's displays keep the arrangement macOS gives them, in that
//! system's own points with y growing downward. A group places each
//! system's displays as one unit by an offset into a shared space. Systems
//! may sit with small gaps or touching only at a corner, but their displays
//! never overlap, and every system stays within the pointer's reach of the
//! rest.

use std::collections::BTreeMap;

use crate::input::{Point, Rect, Side};

/// How close an edge comes before it snaps flush or into line.
pub const SNAP: f64 = 8.0;
/// How far past its own displays the pointer may travel to reach another
/// system's, so small gaps and corners still connect.
pub const REACH: f64 = 40.0;

/// How long a dragged edge stays in line before dragging on pulls it out.
pub const HOLD_MS: u64 = 250;

/// The most displays a system's list may hold; macOS lists at most 16.
pub const MAX_DISPLAYS: usize = 16;
/// The largest display edge or coordinate accepted, in points.
const MAX_EXTENT: f64 = 1_000_000.0;

/// `displays` as a member reported them, if they could be real: at most
/// `MAX_DISPLAYS`, each with a finite position and a finite size of at least
/// one point, which `clamp` needs.
pub fn plausible(displays: Vec<Rect>) -> Option<Vec<Rect>> {
    let sane = |value: f64| value.is_finite() && value.abs() <= MAX_EXTENT;
    (!displays.is_empty()
        && displays.len() <= MAX_DISPLAYS
        && displays.iter().all(|display| {
            sane(display.x)
                && sane(display.y)
                && sane(display.width)
                && sane(display.height)
                && display.width >= 1.0
                && display.height >= 1.0
        }))
    .then_some(displays)
}

/// Arrangement generations use Unix milliseconds plus a short logical-clock
/// allowance. A peer cannot reserve the far future or exhaust local increments.
pub const MAX_VERSION_AHEAD_MS: u64 = 5 * 60 * 1000;
const MAX_OFFSET: f64 = MAX_EXTENT * 3.0 * crate::service::MAX_GROUP as f64;

pub fn plausible_offsets<K: Copy + Ord>(offsets: &[(K, Offset)]) -> bool {
    let keys: std::collections::BTreeSet<_> = offsets.iter().map(|(key, _)| *key).collect();
    keys.len() == offsets.len()
        && offsets
            .iter()
            .all(|(_, (x, y))| x.is_finite() && y.is_finite() && x.abs() <= MAX_OFFSET && y.abs() <= MAX_OFFSET)
}

pub fn plausible_arrangement<K: Copy + Ord>(version: u64, offsets: &[(K, Offset)], now: u64) -> bool {
    version != u64::MAX && version <= now.saturating_add(MAX_VERSION_AHEAD_MS) && plausible_offsets(offsets)
}

/// Where a system's displays sit in the group's space.
pub type Offset = (f64, f64);

#[derive(Debug, Clone, PartialEq)]
pub struct Member<K> {
    pub key: K,
    /// In the system's own coordinates.
    pub displays: Vec<Rect>,
    pub offset: Offset,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Group<K> {
    pub members: Vec<Member<K>>,
}

impl<K: Copy + PartialEq> Group<K> {
    /// Members outside this system's connected component. Small gaps and
    /// corner contact use the same ray entry geometry as pointer crossing.
    pub fn unreachable(&self, from: K) -> Vec<K> {
        let mut reached = vec![from];
        loop {
            let before = reached.len();
            for member in &self.members {
                if reached.contains(&member.key) {
                    continue;
                }
                let connects = self.members.iter().filter(|m| reached.contains(&m.key)).any(|m| {
                    m.displays.iter().any(|a| {
                        member
                            .displays
                            .iter()
                            .any(|b| touching(&shifted(a, m.offset), &shifted(b, member.offset)))
                    })
                });
                if connects {
                    reached.push(member.key);
                }
            }
            if before == reached.len() {
                break;
            }
        }
        self.members
            .iter()
            .filter(|m| !reached.contains(&m.key))
            .map(|m| m.key)
            .collect()
    }
    /// A group of one system, at the origin.
    pub fn alone(key: K, displays: Vec<Rect>) -> Self {
        Self {
            members: vec![Member {
                key,
                displays,
                offset: (0.0, 0.0),
            }],
        }
    }

    fn member(&self, key: K) -> Option<&Member<K>> {
        self.members.iter().find(|member| member.key == key)
    }

    /// `point` on `key`'s displays, in the group's space.
    pub fn to_shared(&self, key: K, point: Point) -> Option<Point> {
        let offset = self.member(key)?.offset;
        Some((point.0 + offset.0, point.1 + offset.1))
    }

    /// `point` in the group's space, in `key`'s own coordinates.
    pub fn to_own(&self, key: K, point: Point) -> Option<Point> {
        let offset = self.member(key)?.offset;
        Some((point.0 - offset.0, point.1 - offset.1))
    }

    /// The center of `key`'s main display, in its own coordinates. macOS
    /// puts the main display's top-left corner at the origin.
    pub fn home(&self, key: K) -> Option<Point> {
        let displays = &self.member(key)?.displays;
        let main = displays
            .iter()
            .find(|display| display.x == 0.0 && display.y == 0.0)
            .or_else(|| displays.first())?;
        Some((main.x + main.width / 2.0, main.y + main.height / 2.0))
    }

    /// Whether `point`, in `key`'s own coordinates, is on one of its displays.
    pub fn on_display(&self, key: K, point: Point) -> bool {
        self.member(key)
            .is_some_and(|member| member.displays.iter().any(|display| contains(display, point)))
    }

    /// The point on `key`'s displays nearest `point`, both in its own
    /// coordinates.
    pub fn nearest(&self, key: K, point: Point) -> Point {
        let Some(member) = self.member(key) else {
            return point;
        };
        member
            .displays
            .iter()
            .map(|display| clamp(display, point))
            .min_by(|a, b| {
                let distance = |p: &Point| (p.0 - point.0).powi(2) + (p.1 - point.1).powi(2);
                distance(a).total_cmp(&distance(b))
            })
            .unwrap_or(point)
    }

    /// Displays of every system but `key`, in the group's space.
    fn others(&self, key: K) -> impl Iterator<Item = Rect> + '_ {
        self.members
            .iter()
            .filter(move |member| member.key != key)
            .flat_map(|member| {
                member
                    .displays
                    .iter()
                    .map(move |display| shifted(display, member.offset))
            })
    }

    /// Whether `key`'s displays at `offset` would overlap another system's.
    pub fn overlaps(&self, key: K, offset: Offset) -> bool {
        let Some(member) = self.member(key) else {
            return false;
        };
        member
            .displays
            .iter()
            .any(|display| self.others(key).any(|other| overlap(&shifted(display, offset), &other)))
    }

    /// Where `key`'s displays go when dropped at `wanted`: snapped flush or
    /// into line when an edge comes within `SNAP`, moved the shortest
    /// distance that clears every other system, then kept within reach of
    /// `from`.
    pub fn place(&self, from: K, key: K, wanted: Offset) -> Offset {
        self.reconnect(from, key, self.settle(key, self.snap(key, wanted)))
    }

    /// `wanted` if `key` there overlaps no other system and leaves every
    /// member `from` reaches now still reachable; otherwise the nearest
    /// offset that does, flush against a display `from` reaches. Members
    /// already out of reach do not hold `key` back. Keeps the current
    /// offset if it fits and no flush placement does; `wanted` when neither
    /// is possible.
    pub fn reconnect(&self, from: K, key: K, wanted: Offset) -> Offset {
        let Some(member) = self.member(key) else {
            return wanted;
        };
        let stranded = self.unreachable(from);
        let fits = |offset: Offset| {
            !self.overlaps(key, offset)
                && self
                    .moved(key, offset)
                    .unreachable(from)
                    .iter()
                    .all(|other| *other != key && stranded.contains(other))
        };
        if fits(wanted) {
            return wanted;
        }
        let reached: Vec<Rect> = self
            .members
            .iter()
            .filter(|other| other.key != key && !stranded.contains(&other.key))
            .flat_map(|other| other.displays.iter().map(|display| shifted(display, other.offset)))
            .collect();
        let others: Vec<Rect> = self.others(key).collect();
        let mut candidates = Vec::new();
        for display in &member.displays {
            for target in &reached {
                // where the two meet edge to edge, from one end to the other
                let (across, down) = clear_of(display, target);
                // along a flush edge: nearest `wanted`, at either end, or
                // just clear of any display in the way
                let along = |[low, high]: [f64; 2], wanted: f64, ends: &dyn Fn(&Rect) -> [f64; 2]| {
                    let mut stops = vec![wanted.clamp(low, high), low, high];
                    stops.extend(others.iter().flat_map(ends).filter(|stop| (low..=high).contains(stop)));
                    stops
                };
                let ys = along(down, wanted.1, &|other| clear_of(display, other).1);
                let xs = along(across, wanted.0, &|other| clear_of(display, other).0);
                for x in across {
                    candidates.extend(ys.iter().map(|&y| (x, y)));
                }
                for y in down {
                    candidates.extend(xs.iter().map(|&x| (x, y)));
                }
            }
        }
        let distance = |(x, y): &Offset| (x - wanted.0).powi(2) + (y - wanted.1).powi(2);
        candidates.sort_by(|a, b| distance(a).total_cmp(&distance(b)));
        candidates
            .into_iter()
            .find(|&candidate| fits(candidate))
            .or_else(|| fits(member.offset).then_some(member.offset))
            .unwrap_or(wanted)
    }

    /// This group with `key`'s displays at `offset`.
    fn moved(&self, key: K, offset: Offset) -> Self {
        let mut group = self.clone();
        for member in &mut group.members {
            if member.key == key {
                member.offset = offset;
            }
        }
        group
    }

    /// The offset nearest `wanted` at which `key` overlaps no other system.
    /// The nearest clear point lies on the boundary of what each pair of
    /// displays rules out, so it is `wanted` moved onto one of those edges
    /// along an axis, or onto a corner of two.
    pub fn settle(&self, key: K, wanted: Offset) -> Offset {
        if !self.overlaps(key, wanted) {
            return wanted;
        }
        let Some(member) = self.member(key) else {
            return wanted;
        };
        let (mut xs, mut ys) = (vec![wanted.0], vec![wanted.1]);
        for display in &member.displays {
            for other in self.others(key) {
                let (across, down) = clear_of(display, &other);
                xs.extend(across);
                ys.extend(down);
            }
        }
        let distance = |(x, y): Offset| (x - wanted.0).powi(2) + (y - wanted.1).powi(2);
        xs.iter()
            .flat_map(|&x| ys.iter().map(move |&y| (x, y)))
            .filter(|&candidate| !self.overlaps(key, candidate))
            .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
            .unwrap_or(wanted)
    }

    /// `wanted`, with each axis moved to make an edge flush with or in line
    /// with another system's when it is within `SNAP`.
    pub fn snap(&self, key: K, wanted: Offset) -> Offset {
        let (xs, ys) = self.lines(key);
        (nearest(wanted.0, &xs, SNAP), nearest(wanted.1, &ys, SNAP))
    }

    /// The offsets on each axis that put an edge of `key`'s displays flush
    /// with or in line with another system's.
    fn lines(&self, key: K) -> (Vec<f64>, Vec<f64>) {
        let Some(member) = self.member(key) else {
            return (Vec::new(), Vec::new());
        };
        let others: Vec<Rect> = self.others(key).collect();
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        for display in &member.displays {
            for other in &others {
                // flush either side, or left or right edges in line
                xs.extend(
                    [
                        other.x - display.width,
                        other.x + other.width,
                        other.x,
                        other.x + other.width - display.width,
                    ]
                    .map(|edge| edge - display.x),
                );
                ys.extend(
                    [
                        other.y - display.height,
                        other.y + other.height,
                        other.y,
                        other.y + other.height - display.height,
                    ]
                    .map(|edge| edge - display.y),
                );
            }
        }
        (xs, ys)
    }

    /// Where the pointer goes when it moves by `delta` from `at`, in the
    /// group's space, on `from`'s displays: `None` while it stays on them,
    /// or the system it reaches and the point it enters at. It continues in
    /// its direction of travel up to `REACH` past where it was headed, and
    /// enters the first display it meets if that belongs to another system.
    pub fn exit(&self, from: K, at: Point, delta: Point) -> Option<(K, Point)> {
        let length = delta.0.hypot(delta.1);
        if length == 0.0 {
            return None;
        }
        let headed = (at.0 + delta.0, at.1 + delta.1);
        let own = self.member(from)?;
        if own
            .displays
            .iter()
            .any(|display| contains(&shifted(display, own.offset), headed))
        {
            return None;
        }
        let direction = (delta.0 / length, delta.1 / length);
        let mut first: Option<(f64, K, Rect)> = None;
        for member in &self.members {
            for display in &member.displays {
                let display = shifted(display, member.offset);
                if let Some(t) = entry(&display, at, direction)
                    && t <= length + REACH
                    && first.is_none_or(|(nearest, _, _)| t < nearest)
                {
                    first = Some((t, member.key, display));
                }
            }
        }
        let (t, key, display) = first?;
        if key == from {
            return None;
        }
        let hit = (at.0 + direction.0 * t, at.1 + direction.1 * t);
        Some((key, clamp(&display, hit)))
    }
}

/// One system's view of a group: every present member's displays, and the
/// offsets every member agrees on. The greatest `(version, author)` wins, so
/// all members converge on one arrangement whoever changes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Placement<K> {
    me: K,
    displays: BTreeMap<K, Vec<Rect>>,
    offsets: BTreeMap<K, Offset>,
    version: (u64, K),
}

impl<K: Copy + Ord> Placement<K> {
    pub fn new(me: K, displays: Vec<Rect>) -> Self {
        Self {
            me,
            displays: BTreeMap::from([(me, displays)]),
            offsets: BTreeMap::from([(me, (0.0, 0.0))]),
            version: (0, me),
        }
    }

    pub fn me(&self) -> K {
        self.me
    }

    /// Makes this a new version authored here, newer than any seen before
    /// even within the same millisecond.
    fn bump(&mut self, now: u64) {
        self.version = (now.max(self.version.0.saturating_add(1)), self.me);
    }

    /// Which arrangement this is; changes whenever it does.
    pub fn version(&self) -> (u64, K) {
        self.version
    }

    /// Records a member's displays; returns whether they changed.
    pub fn show(&mut self, key: K, displays: Vec<Rect>) -> bool {
        self.displays.insert(key, displays.clone()) != Some(displays)
    }

    /// A member left; its offset is kept for when it returns.
    pub fn remove(&mut self, key: K) -> bool {
        key != self.me && self.displays.remove(&key).is_some()
    }

    /// Every present member and its displays.
    pub fn setup(&self) -> crate::recall::Setup<K> {
        crate::recall::setup(self.displays.iter().map(|(key, displays)| (*key, displays.as_slice())))
    }

    /// Where each present member with an agreed offset sits.
    pub fn placed(&self) -> BTreeMap<K, Offset> {
        self.displays
            .keys()
            .filter_map(|key| Some((*key, *self.offsets.get(key)?)))
            .collect()
    }

    /// Puts members at `offsets`, as a new version if that moved any.
    pub fn restore(&mut self, offsets: &[(K, Offset)], now: u64) -> bool {
        let mut moved = false;
        for (key, offset) in offsets {
            moved = self.offsets.insert(*key, *offset) != Some(*offset) || moved;
        }
        if moved {
            self.bump(now);
        }
        moved
    }

    /// Restores one member clear of the others and within reach, keeping
    /// every other member's offset unchanged. The local member may move
    /// here when its display setup returns.
    pub fn restore_member(&mut self, key: K, wanted: Offset, now: u64) -> bool {
        let Some(displays) = self.displays.get(&key) else {
            return false;
        };
        let mut group = self.group();
        if group.member(key).is_none() {
            group.members.push(Member {
                key,
                displays: displays.clone(),
                offset: wanted,
            });
        }
        let offset = group.reconnect(self.me, key, group.settle(key, wanted));
        self.restore(&[(key, offset)], now)
    }

    /// Present members with displays but no agreed offset.
    pub fn unplaced(&self) -> Vec<K> {
        self.displays
            .keys()
            .filter(|key| !self.offsets.contains_key(key))
            .copied()
            .collect()
    }

    /// Puts `key`'s displays on `side` of this system's, clear of every
    /// other member, as a new version of the arrangement. `now` orders it
    /// after anything seen before.
    pub fn place(&mut self, key: K, side: Side, now: u64) -> bool {
        let (Some(mine), Some(theirs)) = (self.displays.get(&self.me), self.displays.get(&key)) else {
            return false;
        };
        let origin = self.offsets.get(&self.me).copied().unwrap_or((0.0, 0.0));
        let beside = beside(mine, theirs, side);
        let wanted = (origin.0 + beside.0, origin.1 + beside.1);
        let mut group = self.group();
        group.members.retain(|member| member.key != key);
        group.members.push(Member {
            key,
            displays: theirs.clone(),
            offset: wanted,
        });
        let settled = group.reconnect(self.me, key, group.settle(key, wanted));
        self.offsets.insert(key, settled);
        self.bump(now);
        true
    }

    /// Puts `key`'s displays where a person dropped them, snapped and clear
    /// of every other member, as a new version. This system stays where it
    /// is. Returns whether that moved them.
    pub fn put(&mut self, key: K, wanted: Offset, now: u64) -> bool {
        if key == self.me || !self.displays.contains_key(&key) {
            return false;
        }
        let mut group = self.group();
        if !group.members.iter().any(|member| member.key == key) {
            group.members.push(Member {
                key,
                displays: self.displays[&key].clone(),
                offset: wanted,
            });
        }
        let placed = group.place(self.me, key, wanted);
        if self.offsets.get(&key) == Some(&placed) {
            return false;
        }
        self.offsets.insert(key, placed);
        self.bump(now);
        true
    }

    /// Moves any member that now overlaps another or is out of reach, as
    /// after a display was added, removed or resized, the shortest way clear
    /// and back within reach, as a new version.
    pub fn repair(&mut self, now: u64) -> bool {
        let me = self.me;
        self.repair_using(now, |group, key, offset| {
            group.reconnect(me, key, group.settle(key, offset))
        })
    }

    /// Remove overlap from an adopted snapshot without closing gaps left by a
    /// departed bridge. Those remaining members keep their chosen positions.
    pub fn repair_overlaps(&mut self, now: u64) -> bool {
        self.repair_using(now, |group, key, offset| {
            if group.overlaps(key, offset) {
                group.settle(key, offset)
            } else {
                offset
            }
        })
    }

    fn repair_using(&mut self, now: u64, fix: impl Fn(&Group<K>, K, Offset) -> Offset) -> bool {
        let mut moved = false;
        for key in self.offsets.keys().copied().collect::<Vec<_>>() {
            if key == self.me {
                continue;
            }
            let group = self.group();
            let Some(offset) = group.member(key).map(|member| member.offset) else {
                continue;
            };
            let fixed = fix(&group, key, offset);
            if fixed != offset {
                self.offsets.insert(key, fixed);
                moved = true;
            }
        }
        if moved {
            self.bump(now);
        }
        moved
    }

    /// Adopts another member's arrangement if it is newer; returns whether
    /// anything changed.
    pub fn adopt(&mut self, version: u64, author: K, offsets: &[(K, Offset)], now: u64) -> bool {
        // Startup snapshots may describe a subgroup that has not learned about
        // this system yet. Wait for its complete snapshot without deleting our
        // usable local placement or disconnecting that peer.
        if !offsets.iter().any(|(key, _)| *key == self.me)
            || !plausible_arrangement(version, offsets, now)
            || (version, author) <= self.version
        {
            return false;
        }
        self.version = (version, author);
        let offsets: BTreeMap<K, Offset> = offsets.iter().copied().collect();
        let changed = offsets != self.offsets;
        self.offsets = offsets;
        changed
    }

    /// The arrangement to send, as `(version, author, offsets)`.
    pub fn message(&self) -> (u64, K, Vec<(K, Offset)>) {
        (
            self.version.0,
            self.version.1,
            self.offsets.iter().map(|(key, offset)| (*key, *offset)).collect(),
        )
    }

    /// The present members that have an offset.
    pub fn group(&self) -> Group<K> {
        Group {
            members: self
                .displays
                .iter()
                .filter_map(|(key, displays)| {
                    Some(Member {
                        key: *key,
                        displays: displays.clone(),
                        offset: *self.offsets.get(key)?,
                    })
                })
                .collect(),
        }
    }
}

/// Holds a dragged member's edges in line with another system's as a drag
/// passes them. Each axis catches the nearest line within `reach`, keeps it
/// for at least `HOLD_MS`, and lets go once dragged past `reach` after that.
/// A line let go of is not caught again in the same drag, so an edge can
/// rest just out of line.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Detent {
    x: Catch,
    y: Catch,
}

impl Detent {
    /// Where `key`'s displays are shown when dragged to `wanted` at `now`,
    /// in milliseconds.
    pub fn follow<K: Copy + PartialEq>(
        &mut self,
        group: &Group<K>,
        key: K,
        wanted: Offset,
        reach: f64,
        now: u64,
    ) -> Offset {
        let (xs, ys) = group.lines(key);
        (
            self.x.follow(wanted.0, &xs, reach, now),
            self.y.follow(wanted.1, &ys, reach, now),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct Catch {
    /// The line held and when it was caught.
    held: Option<(f64, u64)>,
    released: Vec<f64>,
}

impl Catch {
    fn follow(&mut self, wanted: f64, lines: &[f64], reach: f64, now: u64) -> f64 {
        if let Some((line, since)) = self.held {
            if (wanted - line).abs() <= reach || now.saturating_sub(since) < HOLD_MS {
                return line;
            }
            self.held = None;
            self.released.push(line);
        }
        let open: Vec<f64> = lines
            .iter()
            .copied()
            .filter(|line| !self.released.contains(line))
            .collect();
        let line = nearest(wanted, &open, reach);
        if line != wanted {
            self.held = Some((line, now));
        }
        line
    }
}

/// The line in `lines` nearest `current` within `within`, or `current`.
fn nearest(current: f64, lines: &[f64], within: f64) -> f64 {
    lines
        .iter()
        .copied()
        .filter(|line| (line - current).abs() <= within)
        .min_by(|a, b| (a - current).abs().total_cmp(&(b - current).abs()))
        .unwrap_or(current)
}

/// The offset that puts `peer`'s displays on `side` of `local`'s, flush
/// and aligned at the top or left. `local` sits at the origin.
pub fn beside(local: &[Rect], peer: &[Rect], side: Side) -> Offset {
    let (Some(here), Some(there)) = (bounds(local), bounds(peer)) else {
        return (0.0, 0.0);
    };
    let (x, y) = match side {
        Side::Right => (here.x + here.width, here.y),
        Side::Left => (here.x - there.width, here.y),
        Side::Above => (here.x, here.y - there.height),
        Side::Below => (here.x, here.y + here.height),
    };
    (x - there.x, y - there.y)
}

/// The smallest rectangle around `displays`.
pub fn bounds(displays: &[Rect]) -> Option<Rect> {
    let first = displays.first()?;
    let (mut left, mut top, mut right, mut bottom) = (first.x, first.y, first.x + first.width, first.y + first.height);
    for display in &displays[1..] {
        left = left.min(display.x);
        top = top.min(display.y);
        right = right.max(display.x + display.width);
        bottom = bottom.max(display.y + display.height);
    }
    Some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

/// `display` moved by `offset`.
pub fn shifted(display: &Rect, offset: Offset) -> Rect {
    Rect {
        x: display.x + offset.0,
        y: display.y + offset.1,
        ..*display
    }
}

/// Whether two displays meet or are separated by a gap the pointer can cross.
pub(crate) fn touching(a: &Rect, b: &Rect) -> bool {
    let closest = |low: f64, high: f64, other_low: f64, other_high: f64| {
        if high < other_low {
            (high, other_low)
        } else if other_high < low {
            (low, other_high)
        } else {
            let shared = low.max(other_low);
            (shared, shared)
        }
    };
    let (ax, bx) = closest(a.x, a.x + a.width, b.x, b.x + b.width);
    let (ay, by) = closest(a.y, a.y + a.height, b.y, b.y + b.height);
    let length = (bx - ax).hypot(by - ay);
    if length == 0.0 {
        return true;
    }
    entry(b, (ax, ay), ((bx - ax) / length, (by - ay) / length)).is_some_and(|distance| distance <= REACH)
}

/// The offsets at which `display`, in its own coordinates, sits just clear
/// of `other`: left or right of it, and above or below it.
fn clear_of(display: &Rect, other: &Rect) -> ([f64; 2], [f64; 2]) {
    (
        [other.x - display.x - display.width, other.x + other.width - display.x],
        [other.y - display.y - display.height, other.y + other.height - display.y],
    )
}

/// Sharing more than an edge or a corner.
fn overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn contains(display: &Rect, point: Point) -> bool {
    point.0 >= display.x
        && point.0 < display.x + display.width
        && point.1 >= display.y
        && point.1 < display.y + display.height
}

/// How far along `direction` from `origin`, which is outside `display`,
/// the path enters it.
fn entry(display: &Rect, origin: Point, direction: Point) -> Option<f64> {
    if contains(display, origin) {
        return None;
    }
    let mut near = f64::NEG_INFINITY;
    let mut far = f64::INFINITY;
    for (start, step, low, high) in [
        (origin.0, direction.0, display.x, display.x + display.width),
        (origin.1, direction.1, display.y, display.y + display.height),
    ] {
        if step == 0.0 {
            if start < low || start >= high {
                return None;
            }
        } else {
            let (a, b) = ((low - start) / step, (high - start) / step);
            near = near.max(a.min(b));
            far = far.min(a.max(b));
        }
    }
    (near <= far && near >= 0.0).then_some(near)
}

fn clamp(display: &Rect, point: Point) -> Point {
    (
        point.0.clamp(display.x, display.x + display.width - 1.0),
        point.1.clamp(display.y, display.y + display.height - 1.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrangement_clock_and_coordinate_limits_have_explicit_boundaries() {
        let offsets = [(0u8, (0.0, 0.0))];
        assert!(plausible_arrangement(1_000 + MAX_VERSION_AHEAD_MS, &offsets, 1_000));
        assert!(!plausible_arrangement(1_001 + MAX_VERSION_AHEAD_MS, &offsets, 1_000));
        assert!(!plausible_arrangement(u64::MAX, &offsets, u64::MAX));
        assert!(plausible_offsets(&[(0u8, (MAX_OFFSET, -MAX_OFFSET))]));
        assert!(!plausible_offsets(&[(0u8, (MAX_OFFSET + 1.0, 0.0))]));
        assert!(!plausible_offsets(&[(0u8, (0.0, 0.0)), (0, (1.0, 0.0))]));
    }

    #[test]
    fn a_partial_startup_snapshot_cannot_erase_the_local_system() {
        let mut placement = Placement::new(0u8, vec![LAPTOP]);
        placement.show(1, vec![PEER]);
        placement.adopt(10, 0, &[(0, (0.0, 0.0)), (1, (1512.0, 0.0))], 1_000_000);
        placement.adopt(11, 1, &[(1, (0.0, 0.0))], 1_000_000);
        assert!(
            placement.group().members.iter().any(|member| member.key == 0),
            "a valid partial startup snapshot must not erase the local system"
        );
        placement.remove(1);
        assert_eq!(
            placement.group(),
            Group::alone(0, vec![LAPTOP]),
            "a link ending before the full snapshot must leave local routing usable"
        );
    }

    #[test]
    fn remote_arrangements_reject_nonfinite_offsets_without_mutating_placement() {
        for offset in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (f64::NEG_INFINITY, 0.0),
            (f64::MAX, 0.0),
        ] {
            let mut placement = Placement::new(0u8, vec![LAPTOP]);
            placement.show(1, vec![PEER]);
            placement.adopt(10, 0, &[(0, (0.0, 0.0)), (1, (1512.0, 0.0))], 1_000_000);
            let before = placement.message();
            assert!(
                !placement.adopt(11, 1, &[(0, (0.0, 0.0)), (1, offset)], 1_000_000),
                "nonfinite arrangement must be rejected before replacing usable geometry"
            );
            assert_eq!(placement.message(), before);
        }
    }

    #[test]
    fn an_extreme_remote_version_cannot_break_a_later_local_move() {
        let mut placement = Placement::new(0u8, vec![LAPTOP]);
        placement.show(1, vec![PEER]);
        placement.adopt(10, 0, &[(0, (0.0, 0.0)), (1, (1512.0, 0.0))], 1_000_000);
        let adopted = placement.adopt(u64::MAX, 1, &[(0, (0.0, 0.0)), (1, (1512.0, 0.0))], 1_000_000);
        let moved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| placement.put(1, (0.0, 982.0), 12)));
        assert!(
            moved.is_ok(),
            "remote version exhaustion must not panic on a local placement"
        );
        assert!(!adopted, "an exhausted remote version must not replace the local clock");
    }

    #[test]
    fn moving_a_bridge_preserves_members_reached_across_small_gaps() {
        let display = rect(0.0, 0.0, 100.0, 100.0);
        let mut group = Group {
            members: [0, 1, 2]
                .into_iter()
                .map(|key| Member {
                    key,
                    displays: vec![display],
                    offset: (140.0 * f64::from(key), 0.0),
                })
                .collect(),
        };
        assert!(group.unreachable(0).is_empty());
        let offset = group.place(0, 1, (0.0, 100.0));
        group.members[1].offset = offset;
        assert!(!group.overlaps(1, offset));
        assert!(group.unreachable(0).is_empty(), "{group:?}");
    }

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    const LAPTOP: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };
    /// An external display above and to the left of the laptop, as macOS
    /// reports it relative to the laptop as main display.
    const EXTERNAL: Rect = Rect {
        x: -800.0,
        y: -1440.0,
        width: 2560.0,
        height: 1440.0,
    };

    fn group(peer: Vec<Rect>, offset: Offset) -> Group<u8> {
        Group {
            members: vec![
                Member {
                    key: 0,
                    displays: vec![LAPTOP, EXTERNAL],
                    offset: (0.0, 0.0),
                },
                Member {
                    key: 1,
                    displays: peer,
                    offset,
                },
            ],
        }
    }

    const PEER: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1440.0,
        height: 900.0,
    };

    #[test]
    fn touching_is_not_overlapping() {
        assert!(!overlap(&rect(0.0, 0.0, 10.0, 10.0), &rect(10.0, 0.0, 10.0, 10.0)));
        assert!(!overlap(&rect(0.0, 0.0, 10.0, 10.0), &rect(10.0, 10.0, 10.0, 10.0)));
        assert!(overlap(&rect(0.0, 0.0, 10.0, 10.0), &rect(9.0, 9.0, 10.0, 10.0)));
    }

    #[test]
    fn a_clear_spot_is_kept() {
        let group = group(vec![PEER], (1512.0, 0.0));
        assert_eq!(group.settle(1, (1600.0, 50.0)), (1600.0, 50.0));
    }

    #[test]
    fn an_overlap_moves_the_shortest_way_out() {
        let group = group(vec![PEER], (0.0, 0.0));
        // 12 points into the laptop's right edge: out to the right, not up or down
        assert_eq!(group.settle(1, (1500.0, 40.0)), (1512.0, 40.0));
        // just under the laptop's bottom edge, overlapping by 5
        assert_eq!(group.settle(1, (100.0, 977.0)), (100.0, 982.0));
    }

    #[test]
    fn a_settled_spot_never_overlaps_either_display() {
        let group = group(vec![PEER], (0.0, 0.0));
        for wanted in [
            (0.0, 0.0),
            (-500.0, -700.0),
            (700.0, -500.0),
            (1400.0, -1400.0),
            (-900.0, 100.0),
        ] {
            let settled = group.settle(1, wanted);
            assert!(!group.overlaps(1, settled), "{wanted:?} settled at {settled:?}");
        }
    }

    #[test]
    fn the_peer_fits_the_notch_beside_both_displays() {
        // right of the laptop and below the external's overhang
        let group = group(vec![PEER], (0.0, 0.0));
        let settled = group.settle(1, (1600.0, -10.0));
        assert_eq!(settled, (1600.0, 0.0));
    }

    #[test]
    fn edges_snap_flush_and_in_line_only_when_close() {
        let group = group(vec![PEER], (0.0, 0.0));
        assert_eq!(group.snap(1, (1517.0, 6.0)), (1512.0, 0.0));
        assert_eq!(group.snap(1, (1530.0, 30.0)), (1530.0, 30.0));
        assert_eq!(group.place(0, 1, (1507.0, 3.0)), (1512.0, 0.0));
    }

    #[test]
    fn the_pointer_crosses_a_flush_edge_at_the_same_height() {
        let group = group(vec![PEER], (1512.0, 0.0));
        assert_eq!(group.exit(0, (1511.0, 300.0), (4.0, 0.0)), Some((1, (1512.0, 300.0))));
        // and comes back the same way
        assert_eq!(group.exit(1, (1512.0, 300.0), (-4.0, 0.0)), Some((0, (1511.0, 300.0))));
    }

    #[test]
    fn small_gaps_are_crossed_and_wide_ones_are_not() {
        let near = group(vec![PEER], (1512.0 + 30.0, 0.0));
        assert_eq!(near.exit(0, (1511.0, 300.0), (4.0, 0.0)), Some((1, (1542.0, 300.0))));
        let far = group(vec![PEER], (1512.0 + 60.0, 0.0));
        assert_eq!(far.exit(0, (1511.0, 300.0), (4.0, 0.0)), None);
    }

    #[test]
    fn moving_between_a_systems_own_displays_never_crosses() {
        // the peer sits beside the external, above the laptop's right part
        let group = group(vec![PEER], (1760.0, -1440.0));
        // up from the laptop into the external
        assert_eq!(group.exit(0, (100.0, 0.0), (0.0, -4.0)), None);
        // and from the external across to the peer
        assert_eq!(
            group.exit(0, (1759.0, -1000.0), (4.0, 0.0)),
            Some((1, (1760.0, -1000.0)))
        );
    }

    #[test]
    fn a_corner_connects_only_toward_it() {
        // the peer touches the laptop only at its bottom-right corner
        let group = group(vec![PEER], (1512.0, 982.0));
        assert_eq!(group.exit(0, (1511.0, 981.0), (3.0, 3.0)), Some((1, (1512.0, 982.0))));
        assert_eq!(group.exit(0, (1511.0, 500.0), (4.0, 0.0)), None);
    }

    #[test]
    fn an_own_display_in_the_way_wins() {
        let own = vec![rect(0.0, 0.0, 100.0, 100.0), rect(110.0, -100.0, 10.0, 100.0)];
        let peer = Member {
            key: 1,
            displays: vec![rect(125.0, -100.0, 100.0, 100.0)],
            offset: (0.0, 0.0),
        };
        let with = |displays: Vec<Rect>| Group {
            members: vec![
                Member {
                    key: 0,
                    displays,
                    offset: (0.0, 0.0),
                },
                peer.clone(),
            ],
        };
        // the thin display is nearer along the path than the peer
        assert_eq!(with(own.clone()).exit(0, (99.0, 5.0), (4.0, -1.0)), None);
        assert!(with(own[..1].to_vec()).exit(0, (99.0, 5.0), (4.0, -1.0)).is_some());
    }

    #[test]
    fn a_newcomer_is_placed_on_its_side_and_clear_of_the_rest() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.show(2, vec![PEER]);
        assert_eq!(here.unplaced(), [1, 2]);
        assert!(here.place(1, Side::Right, 10));
        assert!(here.place(2, Side::Right, 10));
        let group = here.group();
        assert_eq!(group.members.len(), 3);
        for key in [1, 2] {
            let offset = group.members.iter().find(|m| m.key == key).unwrap().offset;
            assert!(!group.overlaps(key, offset), "{key} at {offset:?}");
        }
        // each change is a newer version, even within the same instant
        assert_eq!(here.message().0, 11);
    }

    #[test]
    fn every_member_converges_on_the_newest_arrangement() {
        let mut a = Placement::new(1u8, vec![LAPTOP]);
        let mut b = Placement::new(2u8, vec![PEER]);
        a.show(2, vec![PEER]);
        b.show(1, vec![LAPTOP]);
        a.place(2, Side::Right, 5);
        b.place(1, Side::Above, 5);
        // b's is the same version but a greater author: it wins on both
        let (version, author, offsets) = b.message();
        assert!(a.adopt(version, author, &offsets, 1_000_000));
        let (version, author, offsets) = a.message();
        assert!(!b.adopt(version, author, &offsets, 1_000_000));
        assert_eq!(a.message(), b.message());
        // an older arrangement never undoes a newer one
        assert!(!a.adopt(1, 9, &[], 1_000_000));
        // the next change here outranks what was adopted
        a.place(2, Side::Below, 0);
        assert!(a.message().0 > b.message().0);
    }

    #[test]
    fn a_display_added_here_pushes_an_overlapping_member_clear() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.place(1, Side::Above, 1);
        assert!(!here.repair(2), "nothing overlaps yet");
        // an external display appears above the laptop, where the peer sits
        here.show(0, vec![LAPTOP, EXTERNAL]);
        assert!(here.repair(3));
        let group = here.group();
        let offset = group.members.iter().find(|m| m.key == 1).unwrap().offset;
        assert!(!group.overlaps(1, offset), "{offset:?}");
    }

    #[test]
    fn a_member_is_put_where_it_was_dropped_snapped_and_clear() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.place(1, Side::Right, 1);
        // dropped near the laptop's left edge, a little low
        assert!(here.put(1, (-1440.0 - 5.0, 4.0), 2));
        let offset = here.group().members.iter().find(|m| m.key == 1).unwrap().offset;
        assert_eq!(offset, (-1440.0, 0.0));
        // dropped on top of the laptop: pushed clear
        assert!(here.put(1, (10.0, 10.0), 3));
        let group = here.group();
        let offset = group.members.iter().find(|m| m.key == 1).unwrap().offset;
        assert!(!group.overlaps(1, offset), "{offset:?}");
        // this system is never moved, and the same spot changes nothing
        assert!(!here.put(0, (500.0, 0.0), 4));
        assert!(!here.put(1, offset, 5));
    }

    #[test]
    fn only_displays_that_could_be_real_are_accepted() {
        assert_eq!(plausible(vec![LAPTOP, EXTERNAL]), Some(vec![LAPTOP, EXTERNAL]));
        let point = Rect {
            width: 1.0,
            height: 1.0,
            ..LAPTOP
        };
        assert_eq!(plausible(vec![point]), Some(vec![point]));
        assert_eq!(plausible(vec![]), None);
        assert_eq!(plausible(vec![LAPTOP; MAX_DISPLAYS + 1]), None);
        for bad in [
            Rect { width: 0.0, ..LAPTOP },
            Rect { width: 0.5, ..LAPTOP },
            Rect { height: 0.5, ..LAPTOP },
            Rect { height: -1.0, ..LAPTOP },
            Rect { x: f64::NAN, ..LAPTOP },
            Rect {
                width: f64::INFINITY,
                ..LAPTOP
            },
            Rect { y: 1e12, ..LAPTOP },
        ] {
            assert_eq!(plausible(vec![LAPTOP, bad]), None, "{bad:?}");
        }
    }

    #[test]
    fn a_member_that_leaves_keeps_its_place() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.place(1, Side::Left, 1);
        assert!(here.remove(1));
        assert!(!here.remove(0), "this system never leaves its own group");
        assert_eq!(here.group().members.len(), 1);
        here.show(1, vec![PEER]);
        assert!(here.unplaced().is_empty());
        assert_eq!(here.group().members.len(), 2);
    }

    #[test]
    fn a_saved_side_becomes_a_flush_offset() {
        let local = [LAPTOP, EXTERNAL];
        assert_eq!(beside(&local, &[PEER], Side::Right), (1760.0, -1440.0));
        assert_eq!(beside(&local, &[PEER], Side::Left), (-800.0 - 1440.0, -1440.0));
        assert_eq!(beside(&local, &[PEER], Side::Below), (-800.0, 982.0));
        assert_eq!(beside(&local, &[PEER], Side::Above), (-800.0, -1440.0 - 900.0));
        let group = group(vec![PEER], beside(&local, &[PEER], Side::Right));
        assert!(!group.overlaps(1, group.members[1].offset));
    }

    #[test]
    fn home_is_the_middle_of_the_main_display() {
        let side = Rect {
            x: -1920.0,
            y: -200.0,
            width: 1920.0,
            height: 1080.0,
        };
        let main = Rect {
            x: 0.0,
            y: 0.0,
            width: 1512.0,
            height: 982.0,
        };
        let group = Group::alone(1, vec![side, main]);
        assert_eq!(group.home(1), Some((756.0, 491.0)), "not the first listed");
        assert_eq!(group.home(2), None);
    }

    #[test]
    fn reachability_includes_edges_gaps_corners_and_connected_chains() {
        let member = |key, x, y| Member {
            key,
            displays: vec![rect(0.0, 0.0, 100.0, 100.0)],
            offset: (x, y),
        };
        for (x, y) in [(100.0, 0.0), (130.0, 0.0), (100.0, 100.0), (120.0, 120.0)] {
            let group = Group {
                members: vec![member(0, 0.0, 0.0), member(1, x, y)],
            };
            assert!(group.unreachable(0).is_empty(), "{x}, {y}");
        }
        for (x, y) in [(141.0, 0.0), (130.0, 130.0)] {
            let group = Group {
                members: vec![member(0, 0.0, 0.0), member(1, x, y)],
            };
            assert_eq!(group.unreachable(0), [1]);
        }
        let mut chain = Group {
            members: vec![member(0, 0.0, 0.0), member(1, 100.0, 0.0), member(2, 200.0, 0.0)],
        };
        assert!(chain.unreachable(0).is_empty());
        chain.members[2].offset = (500.0, 0.0);
        chain.members.push(member(3, 600.0, 0.0));
        assert_eq!(
            chain.unreachable(0),
            [2, 3],
            "two internally connected pairs are still disconnected"
        );
    }

    fn offset_of(placement: &Placement<u8>, key: u8) -> Offset {
        placement.group().members.iter().find(|m| m.key == key).unwrap().offset
    }

    #[test]
    fn a_drop_out_of_reach_lands_on_the_nearest_edge_it_can_reach() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.place(1, Side::Right, 1);
        // far to the right, a little low: back against the laptop's right edge
        assert!(here.put(1, (4000.0, 50.0), 2));
        assert_eq!(offset_of(&here, 1), (1512.0, 50.0));
        // far below and to the left: against the bottom edge, as far left as
        // the edges still meet
        assert!(here.put(1, (-3000.0, 2500.0), 3));
        assert_eq!(offset_of(&here, 1), (-1440.0, 982.0));
        // a small gap is within reach and stays
        assert!(here.put(1, (1512.0 + 30.0, 0.0), 4));
        assert_eq!(offset_of(&here, 1), (1542.0, 0.0));
        assert!(here.group().unreachable(0).is_empty());
    }

    #[test]
    fn a_drop_never_strands_a_member_reached_through_it() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.show(2, vec![PEER]);
        here.adopt(
            1,
            0,
            &[(0, (0.0, 0.0)), (1, (1512.0, 0.0)), (2, (2952.0, 0.0))],
            1_000_000,
        );
        // 2 is reached only through 1; moving 1 below the laptop would strand it
        here.put(1, (0.0, 982.0), 2);
        let group = here.group();
        assert!(group.unreachable(0).is_empty(), "{:?}", group.members);
        assert!(!group.overlaps(1, offset_of(&here, 1)));
    }

    #[test]
    fn a_member_already_out_of_reach_does_not_hold_a_drop_back() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.show(2, vec![PEER]);
        here.adopt(
            1,
            0,
            &[(0, (0.0, 0.0)), (1, (1512.0, 0.0)), (2, (9000.0, 9000.0))],
            1_000_000,
        );
        // 1 is still brought back within reach though 2 never can be
        assert!(here.put(1, (4000.0, 50.0), 2));
        assert_eq!(offset_of(&here, 1), (1512.0, 50.0));
    }

    #[test]
    fn a_display_removed_here_brings_a_stranded_member_back_within_reach() {
        let mut here = Placement::new(0u8, vec![LAPTOP, EXTERNAL]);
        here.show(1, vec![PEER]);
        // above the external display, well clear of the laptop
        here.place(1, Side::Above, 1);
        assert!(!here.repair(2), "nothing is out of reach yet");
        // the external display is unplugged
        here.show(0, vec![LAPTOP]);
        assert_eq!(here.group().unreachable(0), [1]);
        assert!(here.repair(3));
        let group = here.group();
        assert!(group.unreachable(0).is_empty(), "{:?}", offset_of(&here, 1));
        assert!(!group.overlaps(1, offset_of(&here, 1)));
    }

    #[test]
    fn a_dragged_edge_holds_in_line_for_a_beat_then_lets_go() {
        let group = group(vec![PEER], (1512.0, 0.0));
        let reach = 60.0;
        let mut detent = Detent::default();
        // caught flush with the laptop's right edge and in line with its top
        assert_eq!(detent.follow(&group, 1, (1540.0, 20.0), reach, 0), (1512.0, 0.0));
        // dragged well past the line before the beat is up: still held
        assert_eq!(
            detent.follow(&group, 1, (1650.0, 20.0), reach, HOLD_MS - 1),
            (1512.0, 0.0)
        );
        // after the beat, dragging on lets the x line go; y is still within reach
        assert_eq!(detent.follow(&group, 1, (1650.0, 20.0), reach, HOLD_MS), (1650.0, 0.0));
        // coming back near the line let go of does not catch it again
        assert_eq!(
            detent.follow(&group, 1, (1530.0, 20.0), reach, HOLD_MS + 10),
            (1530.0, 0.0)
        );
    }

    #[test]
    fn a_drag_away_from_every_line_follows_the_pointer() {
        let group = group(vec![PEER], (1512.0, 0.0));
        let mut detent = Detent::default();
        assert_eq!(detent.follow(&group, 1, (3000.0, 2000.0), 60.0, 0), (3000.0, 2000.0));
        assert_eq!(detent.follow(&group, 1, (3100.0, 2100.0), 60.0, 1000), (3100.0, 2100.0));
    }
}
