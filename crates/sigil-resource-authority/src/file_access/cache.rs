//! Bounded immutable plan snapshots. Admission tokens, rather than snapshots, are single-use.

use std::collections::BTreeMap;

use super::PlannedFileAccessV1;

pub(super) const MAX_FILE_ACCESS_PLANS: usize = 256;

pub(super) struct PlannedFileCache {
    pub(super) entries: BTreeMap<String, CachedPlan>,
    pub(super) capacity: usize,
    clock: u64,
}

pub(super) struct CachedPlan {
    plan: PlannedFileAccessV1,
    touched: u64,
}

impl Default for PlannedFileCache {
    fn default() -> Self {
        Self {
            entries: BTreeMap::new(),
            capacity: MAX_FILE_ACCESS_PLANS,
            clock: 0,
        }
    }
}

impl PlannedFileCache {
    pub(super) fn insert(&mut self, key: String, plan: PlannedFileAccessV1) {
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(&key) {
            // The hash includes the physical identity and content snapshot. Refreshing a
            // matching entry must not replace data that another admitted call relies on.
            entry.touched = self.clock;
            return;
        }
        if self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.touched)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(
            key,
            CachedPlan {
                plan,
                touched: self.clock,
            },
        );
    }

    pub(super) fn get(&mut self, key: &str) -> Option<&PlannedFileAccessV1> {
        self.clock = self.clock.saturating_add(1);
        let entry = self.entries.get_mut(key)?;
        entry.touched = self.clock;
        Some(&entry.plan)
    }
}
