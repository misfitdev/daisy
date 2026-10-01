//! Where every display of every system in a group sits, and where the
//! pointer goes when it leaves one system's displays.
//!
//! Each system's displays keep the arrangement macOS gives them, in that
//! system's own points with y growing downward. A group places each
//! system's displays as one unit by an offset into a shared space. Systems
//! may sit anywhere, with gaps or touching only at a corner, but their
//! displays never overlap.

use std::collections::BTreeMap;

use crate::input::{Point, Rect, Side};

/// How close an edge comes before it snaps flush or into line.
pub const SNAP: f64 = 8.0;
/// How far past its own displays the pointer may travel to reach another
/// system's, so small gaps and corners still connect.
pub const REACH: f64 = 40.0;

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
    /// into line when an edge comes within `SNAP`, then moved the shortest
    /// distance that clears every other system.
    pub fn place(&self, key: K, wanted: Offset) -> Offset {
        self.settle(key, self.snap(key, wanted))
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
                xs.extend([other.x - display.x - display.width, other.x + other.width - display.x]);
                ys.extend([other.y - display.y - display.height, other.y + other.height - display.y]);
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
        let Some(member) = self.member(key) else {
            return wanted;
        };
        let nearest = |current: f64, targets: &mut dyn Iterator<Item = f64>| {
            targets
                .filter(|target| (target - current).abs() <= SNAP)
                .min_by(|a, b| (a - current).abs().total_cmp(&(b - current).abs()))
                .unwrap_or(current)
        };
        let others: Vec<Rect> = self.others(key).collect();
        let mut x_moves = member.displays.iter().flat_map(|display| {
            others.iter().flat_map(move |other| {
                // flush either side, or left or right edges in line
                [
                    other.x - display.width,
                    other.x + other.width,
                    other.x,
                    other.x + other.width - display.width,
                ]
                .map(|edge| edge - display.x)
            })
        });
        let mut y_moves = member.displays.iter().flat_map(|display| {
            others.iter().flat_map(move |other| {
                [
                    other.y - display.height,
                    other.y + other.height,
                    other.y,
                    other.y + other.height - display.height,
                ]
                .map(|edge| edge - display.y)
            })
        });
        (nearest(wanted.0, &mut x_moves), nearest(wanted.1, &mut y_moves))
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
        let settled = group.settle(key, wanted);
        self.offsets.insert(key, settled);
        self.version = (now.max(self.version.0 + 1), self.me);
        true
    }

    /// Moves any member that now overlaps another, as after a display was
    /// added or resized, the shortest way clear, as a new version.
    pub fn clear_overlaps(&mut self, now: u64) -> bool {
        let mut moved = false;
        for key in self.offsets.keys().copied().collect::<Vec<_>>() {
            if key == self.me {
                continue;
            }
            let group = self.group();
            let Some(offset) = self.offsets.get(&key).copied() else {
                continue;
            };
            if group.overlaps(key, offset) {
                self.offsets.insert(key, group.settle(key, offset));
                moved = true;
            }
        }
        if moved {
            self.version = (now.max(self.version.0 + 1), self.me);
        }
        moved
    }

    /// Adopts another member's arrangement if it is newer; returns whether
    /// anything changed.
    pub fn adopt(&mut self, version: u64, author: K, offsets: &[(K, Offset)]) -> bool {
        if (version, author) <= self.version {
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

fn shifted(display: &Rect, offset: Offset) -> Rect {
    Rect {
        x: display.x + offset.0,
        y: display.y + offset.1,
        ..*display
    }
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
        assert_eq!(group.place(1, (1507.0, 3.0)), (1512.0, 0.0));
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
        assert!(a.adopt(version, author, &offsets));
        let (version, author, offsets) = a.message();
        assert!(!b.adopt(version, author, &offsets));
        assert_eq!(a.message(), b.message());
        // an older arrangement never undoes a newer one
        assert!(!a.adopt(1, 9, &[]));
        // the next change here outranks what was adopted
        a.place(2, Side::Below, 0);
        assert!(a.message().0 > b.message().0);
    }

    #[test]
    fn a_display_added_here_pushes_an_overlapping_member_clear() {
        let mut here = Placement::new(0u8, vec![LAPTOP]);
        here.show(1, vec![PEER]);
        here.place(1, Side::Above, 1);
        assert!(!here.clear_overlaps(2), "nothing overlaps yet");
        // an external display appears above the laptop, where the peer sits
        here.show(0, vec![LAPTOP, EXTERNAL]);
        assert!(here.clear_overlaps(3));
        let group = here.group();
        let offset = group.members.iter().find(|m| m.key == 1).unwrap().offset;
        assert!(!group.overlaps(1, offset), "{offset:?}");
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
}
