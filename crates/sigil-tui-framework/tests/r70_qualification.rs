use std::time::Instant;

use sigil_tui::{
    App, BidiText, CacheHitMetrics, CommittedPresentation, Damage, DamageSummary, FrameMetrics,
    FrameMetricsObserver, HeightIndex, HitTarget, InputEvent, MAX_HIT_GRID_CELLS, NodeId, NodeKey,
    PhaseDurations, Rect, SemanticTheme, Surface, ThemeColor, ThemeRole, UpdateOutcome,
    VirtualSequence,
};

struct QualificationApp;

impl App for QualificationApp {
    type Action = &'static str;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action> {
        match input {
            InputEvent::Key { code } if code == "enter" => UpdateOutcome::Action("submit"),
            InputEvent::Resize { .. } => UpdateOutcome::Redraw(Damage::FULL),
            _ => UpdateOutcome::Ignored,
        }
    }

    fn build_surface(
        &self,
        viewport: Rect,
        generation: u64,
    ) -> Result<Surface, sigil_tui::CoreError> {
        let mut surface = Surface::new(viewport, generation)?;
        surface.push_text(NodeKey::new("unicode")?, viewport, "中文 é אבג 👩‍💻")?;
        Ok(surface)
    }
}

#[test]
fn synthetic_100k_transcript_keeps_resident_materialization_bounded() {
    let started = Instant::now();
    let resident = (0..64).map(|item| format!("item-{item}")).collect();
    let sequence = VirtualSequence::new(7, 49_968, 100_000, resident);
    assert!(sequence.validate().is_ok());
    assert_eq!(sequence.total_items(), 100_000);
    assert_eq!(sequence.resident_len(), 64);
    assert!(sequence.is_bounded_by(64));

    let mut heights = HeightIndex::with_estimate(100_000, 2);
    for item in (0..100_000).step_by(997) {
        heights.set_height(item, 1 + (item % 7) as u32);
    }
    for row in [0, 1, 99_999, heights.total_height() / 2] {
        assert!(heights.locate_row(row).is_some());
    }
    eprintln!(
        "R70_BENCH workload=scroll-100k total={} resident={} elapsed_ns={}",
        sequence.total_items(),
        sequence.resident_len(),
        started.elapsed().as_nanos()
    );
}

#[test]
fn mouse_move_flood_hits_one_committed_grid_without_rebuilding_a_frame() {
    let viewport = Rect::new(0, 0, 120, 40);
    let target = HitTarget::new(NodeId::new(9, 1), viewport, "same-target").expect("target");
    let presentation =
        CommittedPresentation::new(1, viewport, 9, "digest", vec![target]).expect("presentation");
    let mut hits = 0usize;
    for _ in 0..100_000 {
        if presentation.hit_test(60, 20).is_some() {
            hits += 1;
        }
    }
    assert_eq!(hits, 100_000);
    assert_eq!(presentation.hits().len(), 1);
}

#[test]
fn resize_theme_and_unicode_inputs_preserve_framework_contract() {
    let mut app = QualificationApp;
    assert_eq!(
        app.handle_input(InputEvent::Pointer { x: 4, y: 2 }),
        UpdateOutcome::Ignored
    );
    assert_eq!(
        app.handle_input(InputEvent::Resize {
            width: 240,
            height: 80,
        }),
        UpdateOutcome::Redraw(Damage::FULL)
    );
    assert_eq!(
        app.handle_input(InputEvent::Key {
            code: "enter".to_owned(),
        }),
        UpdateOutcome::Action("submit")
    );
    let theme = SemanticTheme::new(
        "qualification",
        1,
        ThemeColor::rgb(0, 0, 0),
        ThemeColor::rgb(255, 255, 255),
        ThemeColor::rgb(128, 128, 128),
        ThemeColor::rgb(80, 160, 255),
        ThemeColor::rgb(255, 80, 80),
        ThemeColor::rgb(80, 220, 120),
    )
    .expect("theme");
    assert_eq!(
        theme.color(ThemeRole::Accent),
        ThemeColor::rgb(80, 160, 255)
    );
    let surface = app
        .build_surface(Rect::new(0, 0, 120, 40), 9)
        .expect("unicode surface");
    assert_eq!(surface.nodes().len(), 1);
    assert!(MAX_HIT_GRID_CELLS >= 120 * 40);
    let bidi = BidiText::new("中文 אבג 👩‍💻").expect("bidi text");
    for (visual, logical) in bidi.map().visual_to_logical().iter().copied().enumerate() {
        assert_eq!(bidi.map().visual_index_for_logical(logical), Some(visual));
    }
}

#[test]
fn frame_metrics_contract_is_host_observable_without_a_telemetry_dependency() {
    struct Observer(Vec<FrameMetrics>);

    impl FrameMetricsObserver for Observer {
        fn observe(&mut self, metrics: FrameMetrics) {
            self.0.push(metrics);
        }
    }

    let damage = Damage::PAINT.union(Damage::INTERACTION);
    let mut metrics = FrameMetrics::new(11, damage);
    metrics.retained_nodes = 100_000;
    metrics.materialized_nodes = 64;
    metrics.measured_nodes = 16;
    metrics.painted_nodes = 16;
    metrics.hit_cells_written = 4_800;
    metrics.changed_cells = 320;
    metrics.cache_hits = CacheHitMetrics {
        hits: 90,
        misses: 10,
    };
    metrics.phase_durations = PhaseDurations {
        application_projection_ns: 1,
        reconcile_ns: 2,
        measure_ns: 3,
        layout_ns: 4,
        virtual_range_ns: 5,
        paint_ns: 6,
        hit_map_ns: 7,
        present_ns: 8,
        input_dispatch_ns: 9,
        present_ack_ns: 10,
    };

    let mut observer = Observer(Vec::new());
    observer.observe(metrics);
    assert_eq!(observer.0.len(), 1);
    assert_eq!(observer.0[0].generation, 11);
    assert_eq!(observer.0[0].damage, DamageSummary::from_damage(damage));
    assert_eq!(observer.0[0].materialized_nodes, 64);
    assert_eq!(observer.0[0].cache_hits.hits, 90);
    assert_eq!(observer.0[0].phase_durations.present_ack_ns, 10);
}
