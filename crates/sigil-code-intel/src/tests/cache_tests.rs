use std::time::{Duration, Instant};

use super::*;

#[test]
fn timed_cache_returns_fresh_values_and_expires_old_values() {
    let now = Instant::now();
    let mut cache = TimedCache::new(Duration::from_secs(20));
    cache.insert("a", vec![1, 2, 3]);

    assert_eq!(cache.get("a"), Some(vec![1, 2, 3]));

    cache
        .values
        .get_mut("a")
        .expect("inserted cache entry should exist")
        .inserted_at = now - Duration::from_secs(21);
    assert_eq!(cache.get("a"), None);
}

#[test]
fn timed_cache_remove_expired_keeps_unexpired_entries() {
    let now = Instant::now();
    let mut cache = TimedCache::new(Duration::from_secs(20));
    cache.values.insert(
        "stale".to_string(),
        TimedValue {
            inserted_at: now - Duration::from_secs(21),
            value: 1,
        },
    );
    cache.values.insert(
        "fresh".to_string(),
        TimedValue {
            inserted_at: now - Duration::from_secs(1),
            value: 2,
        },
    );

    cache.remove_expired();

    assert_eq!(cache.get("stale"), None);
    assert_eq!(cache.get("fresh"), Some(2));
}
