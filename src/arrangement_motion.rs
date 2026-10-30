//! Presentation-only springs and rigid chain links for the arrangement.
//! Nothing here changes the shared layout or participates in input routing.

use std::collections::BTreeMap;

use crate::input::Rect;
use crate::layout::Offset;

/// A slightly underdamped spring with bounded integration steps. Frame time is divided
/// into small steps so a delayed frame cannot make the simulation explode.
#[derive(Debug, Clone, Copy)]
pub struct Spring {
    pub value: f64,
    pub goal: f64,
    velocity: f64,
}

impl Spring {
    pub fn new(value: f64) -> Self {
        Self {
            value,
            goal: value,
            velocity: 0.0,
        }
    }

    pub fn finish(&mut self) {
        self.value = self.goal;
        self.velocity = 0.0;
    }

    pub fn step(&mut self, elapsed: f64, tolerance: f64) -> bool {
        let elapsed = elapsed.clamp(0.0, 0.05);
        let steps = (elapsed * 120.0).ceil().max(1.0) as usize;
        let dt = elapsed / steps as f64;
        for _ in 0..steps {
            self.velocity += (340.0 * (self.goal - self.value) - 29.5 * self.velocity) * dt;
            self.value += self.velocity * dt;
        }
        if (self.goal - self.value).abs() < tolerance && self.velocity.abs() < tolerance * 12.0 {
            self.finish();
            false
        } else {
            true
        }
    }
}

#[derive(Clone, Copy)]
pub struct Target<K> {
    pub key: K,
    pub offset: Offset,
    pub active: bool,
    pub connected: bool,
}

pub struct Body {
    pub x: Spring,
    pub y: Spring,
    pub emphasis: Spring,
    pub connection: Spring,
    pub arrival: Spring,
    /// Seconds since this system gained control, for the bloom and chain tug.
    pub pulse: Option<f64>,
}

impl Body {
    fn finish(&mut self) {
        self.x.finish();
        self.y.finish();
        self.emphasis.finish();
        self.connection.finish();
        self.arrival.finish();
        self.pulse = None;
    }
}

pub struct Motion<K> {
    pub bodies: BTreeMap<K, Body>,
    pub frame: Option<[Spring; 3]>,
}

impl<K> Default for Motion<K> {
    fn default() -> Self {
        Self {
            bodies: BTreeMap::new(),
            frame: None,
        }
    }
}

impl<K: Copy + Ord> Motion<K> {
    /// New members arrive; existing members retain their current drawn pose
    /// when a target changes again mid-animation. Initial and reduced-motion
    /// scenes are complete immediately rather than replaying old events.
    pub fn sync(&mut self, targets: &[Target<K>], animate: bool) {
        let initial = self.bodies.is_empty();
        self.bodies
            .retain(|key, _| targets.iter().any(|target| target.key == *key));
        for target in targets {
            let body = self.bodies.entry(target.key).or_insert_with(|| Body {
                x: Spring::new(target.offset.0),
                y: Spring::new(target.offset.1),
                emphasis: Spring::new(0.0),
                connection: Spring::new(0.0),
                arrival: Spring::new(if animate && !initial { 0.0 } else { 1.0 }),
                pulse: None,
            });
            if target.active && body.emphasis.goal == 0.0 && animate && !initial {
                body.pulse = Some(0.0);
            }
            if !target.active {
                body.pulse = None;
            }
            body.x.goal = target.offset.0;
            body.y.goal = target.offset.1;
            body.emphasis.goal = f64::from(target.active);
            body.connection.goal = f64::from(target.connected);
            body.arrival.goal = 1.0;
        }
        if !animate || initial {
            self.finish();
        }
    }

    pub fn frame(&mut self, scale: f64, origin: Offset, animate: bool) {
        let values = [scale, origin.0, origin.1];
        let frame = self.frame.get_or_insert_with(|| values.map(Spring::new));
        for (spring, goal) in frame.iter_mut().zip(values) {
            spring.goal = goal;
            if !animate {
                spring.finish();
            }
        }
    }

    /// Hold a dragged pose until the authoritative placement comes back.
    pub fn hold(&mut self, key: K, offset: Offset) {
        if let Some(body) = self.bodies.get_mut(&key) {
            body.x = Spring::new(offset.0);
            body.y = Spring::new(offset.1);
            body.arrival.finish();
        }
    }

    pub fn finish(&mut self) {
        for body in self.bodies.values_mut() {
            body.finish();
        }
        if let Some(frame) = &mut self.frame {
            for spring in frame {
                spring.finish();
            }
        }
    }

    /// Returns false once there is no visual work left; the native view can
    /// then invalidate its timer instead of redrawing a sleeping utility.
    pub fn step(&mut self, elapsed: f64) -> bool {
        let mut moving = false;
        for body in self.bodies.values_mut() {
            moving |= body.x.step(elapsed, 0.05);
            moving |= body.y.step(elapsed, 0.05);
            moving |= body.emphasis.step(elapsed, 0.001);
            moving |= body.connection.step(elapsed, 0.001);
            moving |= body.arrival.step(elapsed, 0.001);
            if let Some(age) = &mut body.pulse {
                *age += elapsed.clamp(0.0, 0.05);
                if *age >= 0.8 {
                    body.pulse = None;
                } else {
                    moving = true;
                }
            }
        }
        if let Some(frame) = &mut self.frame {
            moving |= frame[0].step(elapsed, 0.00001);
            moving |= frame[1].step(elapsed, 0.02);
            moving |= frame[2].step(elapsed, 0.02);
        }
        moving
    }
}

/// One physical display in the visual topology. Offline displays keep their
/// geometry but contribute no links, even when their remembered offsets touch.
#[derive(Clone, Copy)]
pub struct Display {
    pub rect: Rect,
    pub connected: bool,
}

pub fn connections(displays: &[Display]) -> Vec<(usize, usize)> {
    let mut edges = Vec::new();
    for (a, from) in displays.iter().enumerate().filter(|(_, display)| display.connected) {
        for (b, to) in displays
            .iter()
            .enumerate()
            .skip(a + 1)
            .filter(|(_, display)| display.connected)
        {
            if crate::layout::touching(&from.rect, &to.rect) {
                edges.push((a, b));
            }
        }
    }
    edges
}

/// Put a label on the quiet side of a connection descending to another screen.
/// Horizontal desks retain the familiar flower-above-name composition.
pub fn caption_above(display: Rect, neighbor: Rect) -> bool {
    neighbor.y + neighbor.height / 2.0 - display.y - display.height / 2.0 > display.height.min(neighbor.height) * 0.35
}

/// Five links straddle the shared edge, leaving captions and flowers clear.
pub fn border_connection(from: Rect, to: Rect) -> Option<(Offset, Offset)> {
    let dx = to.x + to.width / 2.0 - from.x - from.width / 2.0;
    let dy = to.y + to.height / 2.0 - from.y - from.height / 2.0;
    let diagonal = |x: f64, y: f64| Some(((x - dx.signum(), y - dy.signum()), (x + dx.signum(), y + dy.signum())));
    let overlap_x = (from.x + from.width).min(to.x + to.width) - from.x.max(to.x);
    let overlap_y = (from.y + from.height).min(to.y + to.height) - from.y.max(to.y);
    if overlap_y > 1.0 && overlap_x <= 1.0 {
        let y = from.y.max(to.y) + overlap_y / 2.0;
        let x = if from.x < to.x {
            (from.x + from.width + to.x) / 2.0
        } else {
            (to.x + to.width + from.x) / 2.0
        };
        let sign = dx.signum();
        Some(((x - sign, y), (x + sign, y)))
    } else if overlap_x > 1.0 && overlap_y <= 1.0 {
        let x = from.x.max(to.x) + overlap_x / 2.0;
        let y = if from.y < to.y {
            (from.y + from.height + to.y) / 2.0
        } else {
            (to.y + to.height + from.y) / 2.0
        };
        diagonal(x, y)
    } else {
        let x = if from.x < to.x {
            (from.x + from.width + to.x) / 2.0
        } else {
            (to.x + to.width + from.x) / 2.0
        };
        let y = if from.y < to.y {
            (from.y + from.height + to.y) / 2.0
        } else {
            (to.y + to.height + from.y) / 2.0
        };
        let (dx, dy) = ((to.x - from.x).signum(), (to.y - from.y).signum());
        Some(((x - dx, y - dy), (x + dx, y + dy)))
    }
}

/// A corner has no shared edge to carry a five-link chain.
pub fn corner_contact(from: Rect, to: Rect) -> bool {
    let overlap_x = (from.x + from.width).min(to.x + to.width) - from.x.max(to.x);
    let overlap_y = (from.y + from.height).min(to.y + to.height) - from.y.max(to.y);
    overlap_x <= 1.0 && overlap_y <= 1.0
}

/// The primary display keeps its system name; external numbering starts at one.
pub fn display_label(
    name: &str,
    local: bool,
    locked: bool,
    active: bool,
    view: Rect,
    external: Option<usize>,
) -> String {
    let compact = view.width < 140.0 || view.height < 64.0;
    if let Some(number) = external {
        format!("{name}:{number}")
    } else if compact && local {
        if view.width >= 110.0 { "This system" } else { "Here" }.to_owned()
    } else if locked {
        format!("{name} · Locked")
    } else if active {
        format!("{name} · In use")
    } else {
        name.to_owned()
    }
}

/// A whole oval link, fixed in view points rather than stretched to fill a gap.
pub struct ChainLink {
    pub center: Offset,
    pub angle: f64,
}

pub const LINK_SCALE: f64 = 0.18;

/// Two full links on either side of a bisected center link.
pub fn chain(from: Offset, to: Offset, pulse: Option<f64>) -> Vec<ChainLink> {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let distance = dx.hypot(dy);
    if !distance.is_finite() || distance < 1.0 {
        return Vec::new();
    }
    let midpoint = ((from.0 + to.0) / 2.0, (from.1 + to.1) / 2.0);
    let (ux, uy) = (dx / distance, dy / distance);
    (0..5)
        .map(|index| {
            let at = (index as f64 - 2.0) * 40.0 * LINK_SCALE;
            let phase = pulse.map_or(0.0, |age| ((age - index as f64 * 0.022) / 0.22).clamp(0.0, 1.0));
            let lift = (phase * std::f64::consts::PI).sin() * 0.8;
            let turn = (index as f64 - 2.0) * 0.12;
            ChainLink {
                center: (midpoint.0 + ux * at - uy * lift, midpoint.1 + uy * at + ux * lift),
                angle: dy.atan2(dx) + turn + if index % 2 == 1 { std::f64::consts::PI } else { 0.0 },
            }
        })
        .collect()
}

/// Draw only the center link where displays meet at a corner.
pub fn connection_chain(from: Rect, to: Rect, pulse: Option<f64>) -> Vec<ChainLink> {
    let Some((start, end)) = border_connection(from, to) else {
        return Vec::new();
    };
    let mut links = chain(start, end, pulse);
    if corner_contact(from, to) && links.len() == 5 {
        links = vec![links.remove(2)];
    }
    links
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(key: u8, x: f64, active: bool) -> Target<u8> {
        Target {
            key,
            offset: (x, 0.0),
            active,
            connected: true,
        }
    }

    #[test]
    fn border_paths_keep_horizontal_diagonal_and_corner_endpoints() {
        let from = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 80.0,
        };
        for (x, y, expected) in [
            (100.0, 0.0, ((99.0, 40.0), (101.0, 40.0))),
            (-100.0, 0.0, ((1.0, 40.0), (-1.0, 40.0))),
            (100.0, 20.0, ((99.0, 50.0), (101.0, 50.0))),
            (40.0, 80.0, ((69.0, 79.0), (71.0, 81.0))),
            (40.0, -80.0, ((69.0, 1.0), (71.0, -1.0))),
            (100.0, 80.0, ((99.0, 79.0), (101.0, 81.0))),
            (-100.0, 80.0, ((1.0, 79.0), (-1.0, 81.0))),
        ] {
            assert_eq!(
                border_connection(from, Rect { x, y, ..from }),
                Some(expected),
                "neighbor at {x}, {y}"
            );
        }
    }

    #[test]
    fn corner_contacts_draw_one_link_and_shared_edges_draw_five() {
        let from = Rect {
            x: 0.0,
            y: 0.0,
            width: 100.0,
            height: 80.0,
        };
        for (x, y, corner, count) in [
            (100.0, 0.0, false, 5),
            (40.0, 80.0, false, 5),
            (100.0, 80.0, true, 1),
            (-100.0, 80.0, true, 1),
        ] {
            let to = Rect { x, y, ..from };
            assert_eq!(corner_contact(from, to), corner, "neighbor at {x}, {y}");
            assert_eq!(connection_chain(from, to, None).len(), count, "neighbor at {x}, {y}");
        }
    }

    #[test]
    fn display_labels_preserve_precedence_and_compact_thresholds() {
        for (local, locked, active, width, height, external, expected) in [
            (false, false, false, 200.0, 100.0, None, "Studio"),
            (true, false, false, 200.0, 100.0, None, "Studio"),
            (false, false, true, 200.0, 100.0, None, "Studio · In use"),
            (false, true, true, 200.0, 100.0, None, "Studio · Locked"),
            (true, true, true, 110.0, 100.0, None, "This system"),
            (true, false, false, 109.9, 100.0, None, "Here"),
            (true, false, false, 140.0, 64.0, None, "Studio"),
            (true, false, false, 140.0, 63.9, None, "This system"),
            (false, false, false, 100.0, 100.0, None, "Studio"),
            (true, true, true, 100.0, 50.0, Some(1), "Studio:1"),
            (false, false, false, 200.0, 100.0, Some(2), "Studio:2"),
        ] {
            let view = Rect {
                x: 0.0,
                y: 0.0,
                width,
                height,
            };
            assert_eq!(display_label("Studio", local, locked, active, view, external), expected);
        }
    }

    #[test]
    fn interrupted_motion_keeps_its_pose_then_sleeps_at_the_latest_target() {
        let mut motion = Motion::default();
        motion.sync(&[target(1, 0.0, true)], true);
        motion.sync(&[target(1, 1000.0, true)], true);
        for _ in 0..8 {
            motion.step(1.0 / 60.0);
        }
        let x = motion.bodies[&1].x.value;
        assert!(x > 0.0 && x < 1000.0);
        motion.sync(&[target(1, -500.0, true)], true);
        assert_eq!(motion.bodies[&1].x.value, x);
        let mut frames = 0;
        while motion.step(1.0 / 60.0) {
            frames += 1;
            assert!(frames < 180);
        }
        assert_eq!(motion.bodies[&1].x.value, -500.0);
        assert!(!motion.step(1.0 / 60.0));
    }

    #[test]
    fn arrivals_and_control_changes_finish_immediately_without_motion() {
        let mut motion = Motion::default();
        motion.sync(&[target(1, 0.0, true)], true);
        motion.sync(&[target(1, 0.0, false), target(2, 1000.0, true)], true);
        assert_eq!(motion.bodies[&2].arrival.value, 0.0);
        assert_eq!(motion.bodies[&2].pulse, Some(0.0));
        motion.frame(0.1, (20.0, 20.0), true);
        motion.frame(0.05, (100.0, 100.0), true);
        motion.sync(&[target(1, 0.0, false), target(2, 1000.0, true)], false);
        assert_eq!(motion.bodies[&2].arrival.value, 1.0);
        assert_eq!(motion.bodies[&2].emphasis.value, 1.0);
        assert!(motion.bodies[&2].pulse.is_none());
        assert_eq!(motion.frame.unwrap()[0].value, 0.05);
        assert!(!motion.step(1.0 / 60.0));
    }

    #[test]
    fn chain_straddles_the_border_with_five_rigid_links() {
        for end in [(120.0, 0.0), (0.0, 120.0), (100.0, 100.0)] {
            let links = chain((0.0, 0.0), end, None);
            assert_eq!(links.len(), 5);
            assert_eq!(links[2].center, (end.0 / 2.0, end.1 / 2.0));
            for link in [&links[1], &links[3]] {
                let distance = (link.center.0 - links[2].center.0).hypot(link.center.1 - links[2].center.1);
                assert!((distance - 40.0 * LINK_SCALE).abs() < 1e-9);
            }
        }
    }

    #[test]
    fn topology_includes_corners_but_excludes_offline_and_distant_displays() {
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 500.0,
        };
        let mut displays = [
            Display { rect, connected: true },
            Display {
                rect: Rect { x: 1000.0, ..rect },
                connected: true,
            },
            Display {
                rect: Rect { y: 500.0, ..rect },
                connected: true,
            },
        ];
        assert_eq!(connections(&displays), [(0, 1), (0, 2), (1, 2)]);
        assert!(!caption_above(rect, displays[1].rect));
        assert!(caption_above(rect, displays[2].rect));
        assert!(!caption_above(displays[2].rect, rect));
        displays[1].connected = false;
        assert_eq!(connections(&displays), [(0, 2)]);
        displays[2].rect.y = 5000.0;
        assert!(connections(&displays).is_empty());
    }

    #[test]
    fn a_long_frame_stays_bounded_and_removed_members_leave_no_ghosts() {
        let mut motion = Motion::default();
        motion.sync(&[target(1, 0.0, true), target(2, 1000.0, false)], false);
        motion.sync(&[target(1, 5000.0, true), target(2, 1000.0, false)], true);
        motion.step(1000.0);
        assert!(motion.bodies[&1].x.value > 0.0 && motion.bodies[&1].x.value < 5000.0);
        motion.sync(&[target(1, 5000.0, true)], false);
        assert_eq!(motion.bodies.len(), 1);
        assert_eq!(motion.bodies[&1].x.value, 5000.0);
        assert!(!motion.step(1.0 / 60.0));
    }
}
