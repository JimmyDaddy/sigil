use crate::Damage;

/// A renderer-neutral summary of the invalidation that requested a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DamageSummary {
    /// The stable bit representation of the contributing [`Damage`] values.
    pub bits: u8,
    /// Whether the summary contains any invalidation that requests presentation.
    pub frame_requested: bool,
}

impl DamageSummary {
    /// Creates a summary without exposing the framework's internal damage representation.
    pub const fn from_damage(damage: Damage) -> Self {
        Self {
            bits: damage.bits(),
            frame_requested: !damage.is_empty(),
        }
    }

    /// Returns whether the summary contains no requested invalidation.
    pub const fn is_empty(self) -> bool {
        !self.frame_requested
    }
}

/// Counters for reusable framework caches observed while preparing a frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheHitMetrics {
    /// Number of cache lookups that reused an existing value.
    pub hits: u64,
    /// Number of cache lookups that required recomputation.
    pub misses: u64,
}

/// Nanosecond durations for the renderer-neutral frame phases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseDurations {
    /// Time spent obtaining or applying the application projection.
    pub application_projection_ns: u64,
    /// Time spent reconciling retained framework state.
    pub reconcile_ns: u64,
    /// Time spent measuring text and widgets.
    pub measure_ns: u64,
    /// Time spent computing layout.
    pub layout_ns: u64,
    /// Time spent selecting the virtualized visible range.
    pub virtual_range_ns: u64,
    /// Time spent painting the renderer-neutral surface.
    pub paint_ns: u64,
    /// Time spent building or updating the hit map.
    pub hit_map_ns: u64,
    /// Time spent applying and flushing the renderer buffer.
    pub present_ns: u64,
    /// Time spent dispatching input to the application-neutral contract.
    pub input_dispatch_ns: u64,
    /// Time spent waiting for the host's presentation acknowledgement.
    pub present_ack_ns: u64,
}

/// Host-observable counters for one renderer-neutral frame preparation.
///
/// The framework exposes the shape of the observation but does not choose a telemetry backend.
/// Hosts may forward a value to any local metrics sink after a successful or attempted frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameMetrics {
    /// Generation of the surface and presentation represented by this observation.
    pub generation: u64,
    /// Invalidation that caused the frame to be prepared.
    pub damage: DamageSummary,
    /// Number of retained nodes available to reconciliation.
    pub retained_nodes: usize,
    /// Number of nodes materialized for the current viewport.
    pub materialized_nodes: usize,
    /// Number of nodes measured for the frame.
    pub measured_nodes: usize,
    /// Number of nodes painted into the renderer-neutral surface.
    pub painted_nodes: usize,
    /// Number of hit-grid cells written while preparing the frame.
    pub hit_cells_written: usize,
    /// Number of renderer cells changed by the host renderer.
    pub changed_cells: usize,
    /// Cache reuse and recomputation counters.
    pub cache_hits: CacheHitMetrics,
    /// Per-phase durations, in nanoseconds.
    pub phase_durations: PhaseDurations,
}

impl FrameMetrics {
    /// Creates an empty observation for a frame generation and damage value.
    pub const fn new(generation: u64, damage: Damage) -> Self {
        Self {
            generation,
            damage: DamageSummary::from_damage(damage),
            retained_nodes: 0,
            materialized_nodes: 0,
            measured_nodes: 0,
            painted_nodes: 0,
            hit_cells_written: 0,
            changed_cells: 0,
            cache_hits: CacheHitMetrics { hits: 0, misses: 0 },
            phase_durations: PhaseDurations {
                application_projection_ns: 0,
                reconcile_ns: 0,
                measure_ns: 0,
                layout_ns: 0,
                virtual_range_ns: 0,
                paint_ns: 0,
                hit_map_ns: 0,
                present_ns: 0,
                input_dispatch_ns: 0,
                present_ack_ns: 0,
            },
        }
    }
}

/// Receives frame observations without coupling the framework to a telemetry implementation.
pub trait FrameMetricsObserver {
    /// Records one frame observation owned by the caller.
    fn observe(&mut self, metrics: FrameMetrics);
}

#[cfg(test)]
#[path = "tests/metrics_tests.rs"]
mod tests;
