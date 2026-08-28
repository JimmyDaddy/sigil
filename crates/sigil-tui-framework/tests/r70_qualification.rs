use std::time::Instant;

use sigil_tui::{
    App, CommittedPresentation, Damage, HeightIndex, HitTarget, InputEvent, MAX_HIT_GRID_CELLS,
    NodeId, NodeKey, Rect, SemanticTheme, Surface, ThemeColor, ThemeRole, UpdateOutcome,
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
}
