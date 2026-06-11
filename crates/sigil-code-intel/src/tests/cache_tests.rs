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
