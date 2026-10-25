//! Arrangements remembered by the setup they were made for: which systems
//! were present and what displays each had. When a setup comes back, the
//! group starts from the arrangement a person last confirmed for it.
//!
//! A recalled arrangement may add to what a person has done since, but never
//! undo it. Members whose displays are as they were when the arrangement
//! was last confirmed stay where they are; if the remembered arrangement
//! would move them relative to each other, only the member that arrived is
//! placed, beside the member it sat nearest last time.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::input::Rect;
use crate::layout::{Offset, bounds, shifted};

/// How many setups are remembered; the least recently used is forgotten.
pub const MAX_SETUPS: usize = 32;

/// How far apart, in points, two offsets may be and still be the same.
const SAME: f64 = 0.5;

/// Every present member and its displays, sorted, which together say what
/// an arrangement was made for.
pub type Setup<K> = Vec<(K, Vec<Rect>)>;

/// `members` as a setup: sorted by key, each member's displays sorted.
pub fn setup<'a, K: Copy + Ord + 'a>(members: impl IntoIterator<Item = (K, &'a [Rect])>) -> Setup<K> {
    let mut setup: Setup<K> = members
        .into_iter()
        .map(|(key, displays)| {
            let mut displays = displays.to_vec();
            displays.sort_by(|a, b| {
                a.x.total_cmp(&b.x)
                    .then(a.y.total_cmp(&b.y))
                    .then(a.width.total_cmp(&b.width))
                    .then(a.height.total_cmp(&b.height))
            });
            (key, displays)
        })
        .collect();
    setup.sort_by_key(|(key, _)| *key);
    setup
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory<K> {
    /// Least recently used first.
    remembered: Vec<Remembered<K>>,
}

impl<K> Default for Memory<K> {
    fn default() -> Self {
        Self { remembered: Vec::new() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Remembered<K> {
    setup: Setup<K>,
    offsets: Vec<(K, Offset)>,
}

/// What recalling a setup gives the present members.
#[derive(Debug, Clone, PartialEq)]
pub struct Recalled<K> {
    pub offsets: Vec<(K, Offset)>,
    /// Whether this is the whole remembered arrangement, which a person
    /// already confirmed, or only the arrived member placed beside a
    /// neighbor.
    pub whole: bool,
}

impl<K: Copy + Ord> Memory<K> {
    /// Drops malformed persisted entries and keeps the most recently used
    /// setups within the same bound used when remembering new ones.
    pub fn validated(mut self) -> Self {
        self.remembered.retain(|entry| {
            let keys: Vec<K> = entry.setup.iter().map(|(key, _)| *key).collect();
            let placed: BTreeMap<K, Offset> = entry.offsets.iter().copied().collect();
            keys.len() >= 2
                && keys.len() <= crate::service::MAX_GROUP
                && keys.windows(2).all(|pair| pair[0] < pair[1])
                && entry.setup.iter().all(|(_, displays)| {
                    displays.len() <= crate::layout::MAX_DISPLAYS
                        && crate::layout::plausible(displays.clone()).is_some()
                })
                && entry.setup == setup(entry.setup.iter().map(|(key, displays)| (*key, displays.as_slice())))
                && placed.len() == keys.len()
                && entry.offsets.len() == keys.len()
                && keys.iter().all(|key| placed.contains_key(key))
                && placed.values().all(|(x, y)| x.is_finite() && y.is_finite())
        });
        let excess = self.remembered.len().saturating_sub(MAX_SETUPS);
        self.remembered.drain(..excess);
        self
    }

    /// Keeps `offsets` as the arrangement for `setup`, as the most recently
    /// used. Returns whether that changed what is remembered.
    pub fn remember(&mut self, setup: Setup<K>, offsets: &BTreeMap<K, Offset>) -> bool {
        if setup.len() < 2 {
            return false;
        }
        let Some(offsets) = setup
            .iter()
            .map(|(key, _)| offsets.get(key).map(|offset| (*key, *offset)))
            .collect::<Option<Vec<_>>>()
        else {
            return false;
        };
        let entry = Remembered { setup, offsets };
        if self.remembered.last() == Some(&entry) {
            return false;
        }
        self.remembered.retain(|remembered| remembered.setup != entry.setup);
        self.remembered.push(entry);
        let excess = self.remembered.len().saturating_sub(MAX_SETUPS);
        self.remembered.drain(..excess);
        true
    }

    /// Where members go now that the setup is `setup` because `arrived`
    /// joined or its displays changed. `offsets` are where members sit now;
    /// `confirmed` is the setup a person last confirmed. `None` when this
    /// setup was never remembered.
    pub fn recall(
        &mut self,
        setup: &Setup<K>,
        offsets: &BTreeMap<K, Offset>,
        confirmed: Option<&Setup<K>>,
        arrived: K,
    ) -> Option<Recalled<K>> {
        let index = self
            .remembered
            .iter()
            .position(|remembered| remembered.setup == *setup)?;
        let entry = self.remembered.remove(index);
        self.remembered.push(entry.clone());
        let then: BTreeMap<K, Offset> = entry.offsets.iter().copied().collect();
        // members whose displays are as a person last confirmed them
        let kept: Vec<K> = setup
            .iter()
            .filter(|(key, _)| *key != arrived && offsets.contains_key(key))
            .filter(|member| confirmed.is_some_and(|confirmed| confirmed.contains(member)))
            .map(|(key, _)| *key)
            .collect();
        let anchor = kept
            .first()
            .or_else(|| setup.iter().map(|(key, _)| key).find(|key| offsets.contains_key(key)))
            .copied();
        let shift = anchor.map_or((0.0, 0.0), |key| {
            (offsets[&key].0 - then[&key].0, offsets[&key].1 - then[&key].1)
        });
        let unchanged = kept.iter().all(|key| {
            (then[key].0 + shift.0 - offsets[key].0).abs() <= SAME
                && (then[key].1 + shift.1 - offsets[key].1).abs() <= SAME
        });
        if unchanged {
            return Some(Recalled {
                offsets: entry
                    .offsets
                    .iter()
                    .map(|(key, (x, y))| (*key, (x + shift.0, y + shift.1)))
                    .collect(),
                whole: true,
            });
        }
        let displays: BTreeMap<K, &[Rect]> = setup
            .iter()
            .map(|(key, displays)| (*key, displays.as_slice()))
            .collect();
        let theirs = bounds(displays[&arrived]).map(|bounds| shifted(&bounds, then[&arrived]))?;
        let neighbor = kept.iter().copied().min_by(|a, b| {
            let gap = |key: &K| {
                bounds(displays[key])
                    .map(|bounds| shifted(&bounds, then[key]))
                    .map_or(f64::INFINITY, |bounds| gap(&bounds, &theirs))
            };
            gap(a).total_cmp(&gap(b))
        })?;
        let offset = (
            offsets[&neighbor].0 + then[&arrived].0 - then[&neighbor].0,
            offsets[&neighbor].1 + then[&arrived].1 - then[&neighbor].1,
        );
        Some(Recalled {
            offsets: vec![(arrived, offset)],
            whole: false,
        })
    }
}

/// The distance between two rectangles; zero when they touch or overlap.
fn gap(a: &Rect, b: &Rect) -> f64 {
    let dx = (b.x - (a.x + a.width)).max(a.x - (b.x + b.width)).max(0.0);
    let dy = (b.y - (a.y + a.height)).max(a.y - (b.y + b.height)).max(0.0);
    dx.hypot(dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
    }

    const A: u8 = 1;
    const B: u8 = 2;
    const C: u8 = 3;

    fn laptop() -> Vec<Rect> {
        vec![rect(0.0, 0.0, 1512.0, 982.0)]
    }

    /// A laptop with the big monitor above it.
    fn docked() -> Vec<Rect> {
        vec![rect(0.0, 0.0, 1512.0, 982.0), rect(-524.0, -1440.0, 2560.0, 1440.0)]
    }

    fn setup_of(members: &[(u8, Vec<Rect>)]) -> Setup<u8> {
        setup(members.iter().map(|(key, displays)| (*key, displays.as_slice())))
    }

    fn offsets(placed: &[(u8, Offset)]) -> BTreeMap<u8, Offset> {
        placed.iter().copied().collect()
    }

    #[test]
    fn loaded_memory_discards_incomplete_nonfinite_and_implausible_entries() {
        let valid = Remembered {
            setup: setup_of(&[(A, laptop()), (B, laptop())]),
            offsets: vec![(A, (0.0, 0.0)), (B, (1512.0, 0.0))],
        };
        let mut invalid = Vec::new();
        let mut missing = valid.clone();
        missing.offsets.pop();
        invalid.push(missing);
        let mut duplicate = valid.clone();
        duplicate.offsets[1].0 = A;
        invalid.push(duplicate);
        let mut extra = valid.clone();
        extra.offsets.push((C, (0.0, 0.0)));
        invalid.push(extra);
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut bad = valid.clone();
            bad.offsets[0].1.0 = value;
            invalid.push(bad);
        }
        let mut duplicate_member = valid.clone();
        duplicate_member.setup[1].0 = A;
        invalid.push(duplicate_member);
        for displays in [
            vec![],
            vec![laptop()[0]; crate::layout::MAX_DISPLAYS + 1],
            vec![rect(0.0, 0.0, 0.0, 1.0)],
        ] {
            let mut bad = valid.clone();
            bad.setup[0].1 = displays;
            invalid.push(bad);
        }
        for bad in invalid {
            let loaded = Memory {
                remembered: vec![bad, valid.clone()],
            }
            .validated();
            assert_eq!(loaded.remembered, vec![valid.clone()]);
        }
    }

    #[test]
    fn loaded_memory_keeps_only_the_most_recent_valid_setups() {
        let entries = (0..MAX_SETUPS + 3)
            .map(|index| Remembered {
                setup: setup_of(&[(A, vec![rect(0.0, 0.0, 1000.0 + index as f64, 800.0)]), (B, laptop())]),
                offsets: vec![(A, (0.0, 0.0)), (B, (2000.0, 0.0))],
            })
            .collect();
        let loaded = Memory { remembered: entries }.validated();
        assert_eq!(loaded.remembered.len(), MAX_SETUPS);
        assert_eq!(loaded.remembered[0].setup[0].1[0].width, 1003.0);
        assert_eq!(
            loaded.remembered.last().unwrap().setup[0].1[0].width,
            1000.0 + (MAX_SETUPS + 2) as f64
        );
    }

    #[test]
    fn the_same_setup_comes_back_where_it_was_last_confirmed() {
        let mut memory = Memory::default();
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        assert!(memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0)), (B, (1512.0, 0.0))])));
        // b joins on its paired side, the left, with a moved off the origin
        let now = offsets(&[(A, (100.0, 50.0))]);
        let recalled = memory
            .recall(&pair, &now, Some(&setup_of(&[(A, laptop())])), B)
            .unwrap();
        assert!(recalled.whole);
        assert_eq!(recalled.offsets, [(A, (100.0, 50.0)), (B, (1612.0, 50.0))]);
    }

    #[test]
    fn a_setup_never_seen_recalls_nothing() {
        let mut memory = Memory::default();
        memory.remember(
            setup_of(&[(A, laptop()), (B, laptop())]),
            &offsets(&[(A, (0.0, 0.0)), (B, (1512.0, 0.0))]),
        );
        let docked_pair = setup_of(&[(A, docked()), (B, laptop())]);
        assert_eq!(memory.recall(&docked_pair, &offsets(&[(A, (0.0, 0.0))]), None, B), None);
    }

    #[test]
    fn moving_the_monitor_to_another_member_swaps_them_as_remembered() {
        let mut memory = Memory::default();
        // with the monitor on a: c, a, b from left to right
        let a_docked = setup_of(&[(A, docked()), (B, laptop()), (C, laptop())]);
        let c_docked = setup_of(&[(A, laptop()), (B, laptop()), (C, docked())]);
        let a_centered = offsets(&[(C, (-1512.0, 0.0)), (A, (0.0, 0.0)), (B, (1512.0, 0.0))]);
        let c_centered = offsets(&[(A, (-1512.0, 0.0)), (C, (0.0, 0.0)), (B, (1512.0, 0.0))]);
        memory.remember(a_docked.clone(), &a_centered);
        memory.remember(c_docked.clone(), &c_centered);
        // a's monitor moved to c: both changed since a_docked was confirmed
        let recalled = memory.recall(&c_docked, &a_centered, Some(&a_docked), C).unwrap();
        assert!(recalled.whole, "only b is kept, and it has not moved");
        assert_eq!(recalled.offsets.iter().copied().collect::<BTreeMap<_, _>>(), c_centered);
    }

    #[test]
    fn a_late_arrival_never_undoes_what_a_person_arranged_since() {
        let mut memory = Memory::default();
        let all = setup_of(&[(A, laptop()), (B, laptop()), (C, laptop())]);
        // last time: c, a, b
        memory.remember(
            all.clone(),
            &offsets(&[(C, (-1512.0, 0.0)), (A, (0.0, 0.0)), (B, (1512.0, 0.0))]),
        );
        // today b was put below a and confirmed
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        let now = offsets(&[(A, (0.0, 0.0)), (B, (0.0, 982.0))]);
        let recalled = memory.recall(&all, &now, Some(&pair), C).unwrap();
        assert!(!recalled.whole);
        // c sat flush left of a, its nearest neighbor then, and still does
        assert_eq!(recalled.offsets, [(C, (-1512.0, 0.0))]);
    }

    #[test]
    fn remembering_again_replaces_and_the_least_recently_used_is_forgotten() {
        let mut memory = Memory::default();
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0)), (B, (1512.0, 0.0))]));
        assert!(memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0)), (B, (-1512.0, 0.0))])));
        assert!(!memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0)), (B, (-1512.0, 0.0))])));
        let recalled = memory.recall(&pair, &offsets(&[(A, (0.0, 0.0))]), None, B).unwrap();
        assert_eq!(recalled.offsets[1], (B, (-1512.0, 0.0)));
        for size in 0..MAX_SETUPS {
            let other = setup_of(&[(A, vec![rect(0.0, 0.0, 1000.0 + size as f64, 800.0)]), (B, laptop())]);
            memory.remember(other, &offsets(&[(A, (0.0, 0.0)), (B, (2000.0, 0.0))]));
        }
        assert_eq!(memory.recall(&pair, &offsets(&[(A, (0.0, 0.0))]), None, B), None);
    }

    #[test]
    fn recalling_a_setup_keeps_it_from_being_forgotten() {
        let mut memory = Memory::default();
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0)), (B, (1512.0, 0.0))]));
        for size in 0..MAX_SETUPS {
            if size == MAX_SETUPS / 2 {
                assert!(memory.recall(&pair, &offsets(&[(A, (0.0, 0.0))]), None, B).is_some());
            }
            let other = setup_of(&[(A, vec![rect(0.0, 0.0, 1000.0 + size as f64, 800.0)]), (B, laptop())]);
            memory.remember(other, &offsets(&[(A, (0.0, 0.0)), (B, (2000.0, 0.0))]));
        }
        assert!(memory.recall(&pair, &offsets(&[(A, (0.0, 0.0))]), None, B).is_some());
    }

    #[test]
    fn a_lone_system_or_a_member_without_a_place_is_not_remembered() {
        let mut memory = Memory::default();
        assert!(!memory.remember(setup_of(&[(A, laptop())]), &offsets(&[(A, (0.0, 0.0))])));
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        assert!(!memory.remember(pair, &offsets(&[(A, (0.0, 0.0))])));
    }

    #[test]
    fn remembered_setups_survive_a_round_trip_through_a_file() {
        let mut memory = Memory::default();
        memory.remember(
            setup_of(&[(A, docked()), (B, laptop())]),
            &offsets(&[(A, (0.0, 0.0)), (B, (2036.0, -1440.0))]),
        );
        let text = toml::to_string(&memory).unwrap();
        assert_eq!(toml::from_str::<Memory<u8>>(&text).unwrap(), memory);
    }
}
