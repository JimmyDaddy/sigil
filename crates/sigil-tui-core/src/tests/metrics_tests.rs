use super::{DamageSummary, FrameMetrics, FrameMetricsObserver};
use crate::Damage;

#[test]
fn metrics_preserve_damage_and_keep_counters_explicit() {
    let damage = Damage::PAINT.union(Damage::INTERACTION);
    let metrics = FrameMetrics::new(7, damage);
    assert_eq!(metrics.generation, 7);
    assert_eq!(metrics.damage, DamageSummary::from_damage(damage));
    assert!(metrics.damage.frame_requested);
    assert_eq!(metrics.retained_nodes, 0);
    assert_eq!(metrics.phase_durations.present_ack_ns, 0);
}

struct RecordingObserver(Option<FrameMetrics>);

impl FrameMetricsObserver for RecordingObserver {
    fn observe(&mut self, metrics: FrameMetrics) {
        self.0 = Some(metrics);
    }
}

#[test]
fn observer_contract_is_backend_neutral() {
    let expected = FrameMetrics::new(3, Damage::FULL);
    let mut observer = RecordingObserver(None);
    observer.observe(expected);
    assert_eq!(observer.0, Some(expected));
}
