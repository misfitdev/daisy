//! Arrangements a person explicitly saved for exact sets of systems and displays.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::input::Rect;
use crate::layout::Offset;

/// Maximum saved setups; the oldest saved setup is forgotten first.
pub const MAX_SETUPS: usize = 32;

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
    /// Oldest save first.
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

impl<K: Copy + Ord> Memory<K> {
    /// Drops malformed persisted entries and keeps the newest saved setups.
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
                && crate::layout::plausible_offsets(&entry.offsets)
        });
        // A damaged file may contain repeated setups. Keep the last save.
        let mut seen = Vec::new();
        self.remembered.reverse();
        self.remembered.retain(|entry| {
            if seen.contains(&entry.setup) {
                false
            } else {
                seen.push(entry.setup.clone());
                true
            }
        });
        self.remembered.reverse();
        let excess = self.remembered.len().saturating_sub(MAX_SETUPS);
        self.remembered.drain(..excess);
        self
    }

    /// Saves `offsets` for `setup`, replacing any earlier save for it.
    /// Returns whether the saved arrangements changed.
    pub fn remember(&mut self, setup: Setup<K>, offsets: &BTreeMap<K, Offset>) -> bool {
        if setup.len() < 2
            || !crate::layout::plausible_offsets(
                &offsets.iter().map(|(key, offset)| (*key, *offset)).collect::<Vec<_>>(),
            )
        {
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

    /// The saved positions for exactly this set of systems and displays.
    pub fn get(&self, setup: &Setup<K>) -> Option<&[(K, Offset)]> {
        self.remembered
            .iter()
            .rev()
            .find(|entry| entry.setup == *setup)
            .map(|entry| entry.offsets.as_slice())
    }
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
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
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
    fn duplicate_loaded_setup_keeps_the_latest_offsets() {
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        let first = Remembered {
            setup: pair.clone(),
            offsets: vec![(A, (0.0, 0.0)), (B, (1512.0, 0.0))],
        };
        let last = Remembered {
            setup: pair.clone(),
            offsets: vec![(A, (0.0, 0.0)), (B, (-1512.0, 0.0))],
        };
        let memory = Memory {
            remembered: vec![first, last.clone()],
        }
        .validated();
        assert_eq!(memory.remembered, vec![last]);
        assert_eq!(memory.get(&pair).unwrap()[1].1, (-1512.0, 0.0));
    }

    #[test]
    fn exact_setup_is_required_and_resaving_replaces_it() {
        let mut memory = Memory::default();
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        let first = offsets(&[(A, (0.0, 0.0)), (B, (1512.0, 0.0))]);
        assert!(memory.remember(pair.clone(), &first));
        assert_eq!(
            memory.get(&pair),
            Some(first.iter().map(|(k, v)| (*k, *v)).collect::<Vec<_>>().as_slice())
        );
        assert_eq!(memory.get(&setup_of(&[(A, docked()), (B, laptop())])), None);
        let second = offsets(&[(A, (0.0, 0.0)), (B, (-1512.0, 0.0))]);
        assert!(memory.remember(pair.clone(), &second));
        assert_eq!(memory.get(&pair).unwrap()[1], (B, (-1512.0, 0.0)));
    }

    #[test]
    fn incomplete_layout_cannot_be_saved() {
        let mut memory = Memory::default();
        let pair = setup_of(&[(A, laptop()), (B, laptop())]);
        assert!(!memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0))])));
        assert_eq!(memory.get(&pair), None);
    }

    #[test]
    fn saved_setups_survive_a_round_trip() {
        let mut memory = Memory::default();
        let pair = setup_of(&[(A, docked()), (B, laptop())]);
        memory.remember(pair.clone(), &offsets(&[(A, (0.0, 0.0)), (B, (2036.0, -1440.0))]));
        let text = toml::to_string(&memory).unwrap();
        let loaded = toml::from_str::<Memory<u8>>(&text).unwrap();
        assert_eq!(loaded.get(&pair), memory.get(&pair));
    }
}
