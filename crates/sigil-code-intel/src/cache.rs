use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

#[derive(Debug, Clone)]
pub struct TimedCache<V> {
    ttl: Duration,
    values: BTreeMap<String, TimedValue<V>>,
}

#[derive(Debug, Clone)]
struct TimedValue<V> {
    inserted_at: Instant,
    value: V,
}

impl<V: Clone> TimedCache<V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            values: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: impl Into<String>, value: V) {
        self.values.insert(
            key.into(),
            TimedValue {
                inserted_at: Instant::now(),
                value,
            },
        );
    }

    pub fn get(&mut self, key: &str) -> Option<V> {
        self.remove_expired();
        self.values.get(key).map(|value| value.value.clone())
    }

    pub fn values(&mut self) -> Vec<V> {
        self.remove_expired();
        self.values
            .values()
            .map(|value| value.value.clone())
            .collect()
    }

    pub fn remove(&mut self, key: &str) {
        self.values.remove(key);
    }

    pub fn remove_expired(&mut self) {
        let ttl = self.ttl;
        self.values
            .retain(|_, value| value.inserted_at.elapsed() <= ttl);
    }
}

#[cfg(test)]
#[path = "tests/cache_tests.rs"]
mod tests;
