use sigil_tui::{
    App, CommittedPresentation, Damage, FrameMetrics, InputEvent, NodeId, NodeKey, Rect, Surface,
    Text, UpdateOutcome, WidgetKind, WidgetSpec, WidgetTree,
};

struct Consumer {
    label: String,
}

impl App for Consumer {
    type Action = &'static str;

    fn handle_input(&mut self, input: InputEvent) -> UpdateOutcome<Self::Action> {
        match input {
            InputEvent::Key { code } if code == "enter" => UpdateOutcome::Action("activate"),
            InputEvent::Paste(value) => {
                self.label = value;
                UpdateOutcome::Redraw(Damage::PAINT)
            }
            _ => UpdateOutcome::Ignored,
        }
    }

    fn build_surface(
        &self,
        viewport: Rect,
        generation: u64,
    ) -> Result<Surface, sigil_tui::CoreError> {
        let mut surface = Surface::new(viewport, generation)?;
        surface.push_text(
            NodeKey::new("consumer.label")?,
            viewport,
            Text::new(self.label.as_str())?.to_string(),
        )?;
        Ok(surface)
    }
}

#[test]
fn independent_consumer_uses_only_public_framework_contract() {
    let mut consumer = Consumer {
        label: "initial".into(),
    };
    assert_eq!(
        consumer.handle_input(InputEvent::Paste("updated".into())),
        UpdateOutcome::Redraw(Damage::PAINT)
    );
    let surface = consumer
        .build_surface(Rect::new(0, 0, 20, 2), 1)
        .expect("bounded surface");
    assert_eq!(surface.nodes().len(), 1);
    assert_eq!(surface.hit_test(0, 0), Some(&surface.nodes()[0]));
    let presentation = surface
        .committed_presentation(1)
        .expect("presentation should be committed");
    assert!(presentation.hits().is_empty());
}

#[test]
fn surface_rejects_duplicate_keys_and_unbounded_text() {
    let mut surface = Surface::new(Rect::new(0, 0, 10, 2), 1).expect("surface");
    let key = NodeKey::new("same").expect("key");
    surface
        .push_text(key.clone(), Rect::new(0, 0, 2, 1), "one")
        .expect("first node");
    assert!(
        surface
            .push_text(key, Rect::new(0, 1, 2, 1), "two")
            .is_err()
    );
    assert!(Text::new("x".repeat(sigil_tui::MAX_SURFACE_TEXT_BYTES + 1)).is_err());
}

#[test]
fn independent_consumer_can_use_standard_widget_specs_without_sigil_domain_types() {
    let viewport = Rect::new(0, 0, 20, 2);
    let mut tree = WidgetTree::new(viewport, 1).expect("tree");
    tree.push(
        WidgetSpec::new(
            NodeKey::new("todo").expect("key"),
            WidgetKind::VirtualList,
            viewport,
            "one item",
        )
        .expect("widget"),
    )
    .expect("widget should be bounded");
    assert_eq!(tree.surface().nodes().len(), 1);
}

#[test]
fn prepared_render_carries_generation_bound_metrics() {
    let viewport = Rect::new(0, 0, 4, 2);
    let surface = Surface::new(viewport, 7).expect("surface");
    let presentation = CommittedPresentation::new(
        3,
        viewport,
        7,
        "digest",
        vec![sigil_tui::HitTarget::new(NodeId::new(7, 0), viewport, "action").expect("hit target")],
    )
    .expect("presentation");
    let metrics = FrameMetrics::new(7, Damage::PAINT);
    let prepared =
        sigil_tui::PreparedRender::new(7, surface, presentation, metrics).expect("prepared render");
    assert_eq!(prepared.metrics(), &metrics);
}
