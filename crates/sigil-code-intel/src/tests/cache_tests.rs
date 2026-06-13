use std::{thread, time::Duration};

use super::*;

#[test]
fn timed_cache_returns_fresh_values_and_expires_old_values() {
    let mut cache = TimedCache::new(Duration::from_millis(5));
    cache.insert("a", vec![1, 2, 3]);

    assert_eq!(cache.get("a"), Some(vec![1, 2, 3]));

    thread::sleep(Duration::from_millis(10));
    assert_eq!(cache.get("a"), None);
}

#[test]
fn timed_cache_remove_clears_only_matching_entry() {
    let mut cache = TimedCache::new(Duration::from_secs(1));
    cache.insert("a", 1);
    cache.insert("b", 2);

    cache.remove("a");

    assert_eq!(cache.get("a"), None);
    assert_eq!(cache.get("b"), Some(2));
}

#[test]
fn timed_cache_remove_expired_keeps_unexpired_entries() {
    let mut cache = TimedCache::new(Duration::from_millis(20));
    cache.insert("stale", 1);
    thread::sleep(Duration::from_millis(15));
    cache.insert("fresh", 2);
    thread::sleep(Duration::from_millis(10));

    cache.remove_expired();

    assert_eq!(cache.get("stale"), None);
    assert_eq!(cache.get("fresh"), Some(2));
}
