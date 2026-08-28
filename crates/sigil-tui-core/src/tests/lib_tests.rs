use super::{
    CommittedPresentation, HeightIndex, HitTarget, InputEvent, MAX_HIT_GRID_CELLS,
    MAX_INPUT_TEXT_BYTES, MAX_SURFACE_NODES, NodeId, NodeKey, Rect, Surface, VirtualSequence,
};

#[test]
fn surface_nodes_expose_only_validated_read_access() {
    let viewport = Rect::new(0, 0, 10, 2);
    let mut surface = Surface::new(viewport, 1).expect("surface");
    surface
        .push_action(
            NodeKey::new("save").expect("key"),
            viewport,
            "Save",
            NodeKey::new("save.command").expect("binding"),
        )
        .expect("action");
    let node = &surface.nodes()[0];
    assert_eq!(node.id().generation, 1);
    assert_eq!(node.key().as_str(), "save");
    assert_eq!(node.bounds(), viewport);
}

#[test]
fn virtual_sequence_validation_rejects_unusable_generation() {
    let invalid = VirtualSequence::new(0, 0, 1, vec!["item"]);
    assert!(invalid.validate().is_err());

    let valid = VirtualSequence::new(1, 3, 4, vec!["item"]);
    assert!(valid.validate().is_ok());
    assert!(valid.is_bounded_by(MAX_SURFACE_NODES));
    assert_eq!(valid.item_ids()[0].ordinal, 3);
}

#[test]
fn input_validation_rejects_unbounded_payloads() {
    assert!(
        InputEvent::Key {
            code: String::new()
        }
        .validate()
        .is_err()
    );
    assert!(
        InputEvent::Paste("x".repeat(MAX_INPUT_TEXT_BYTES + 1))
            .validate()
            .is_err()
    );
    assert!(
        InputEvent::Resize {
            width: 80,
            height: 24,
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn committed_presentation_uses_the_topmost_target_from_one_dense_grid() {
    let viewport = Rect::new(10, 4, 20, 4);
    let lower = HitTarget::new(NodeId::new(1, 1), viewport, "lower").expect("target");
    let upper = HitTarget::new(NodeId::new(1, 2), Rect::new(12, 5, 4, 2), "upper").expect("target");
    let presentation = CommittedPresentation::new(1, viewport, 1, "digest", vec![lower, upper])
        .expect("presentation");

    assert_eq!(
        presentation.hit_test(10, 4).map(HitTarget::binding),
        Some("lower")
    );
    assert_eq!(
        presentation.hit_test(13, 5).map(HitTarget::binding),
        Some("upper")
    );
    assert!(presentation.hit_test(9, 4).is_none());
}

#[test]
fn committed_presentation_rejects_an_unbounded_dense_grid() {
    let width = (MAX_HIT_GRID_CELLS as f64).sqrt() as u16 + 1;
    let viewport = Rect::new(0, 0, width, width);
    assert!(CommittedPresentation::new(1, viewport, 1, "digest", Vec::new()).is_err());
}

#[test]
fn height_index_locates_variable_rows_with_fenwick_binary_lifting() {
    let mut index = HeightIndex::with_estimate(100_000, 2);
    assert_eq!(index.locate_row(0), Some((0, 0)));
    assert_eq!(index.locate_row(199_999), Some((99_999, 1)));
    assert_eq!(index.set_height(50_000, 7), Some(2));
    assert_eq!(index.locate_row(100_000), Some((50_000, 0)));
    assert!(index.locate_row(index.total_height()).is_none());
}
