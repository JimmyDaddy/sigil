use super::{WidgetKind, WidgetSpec, WidgetTree};
use crate::{NodeKey, Rect};

#[test]
fn standard_widget_declarations_lower_to_bounded_surface_nodes() {
    let viewport = Rect::new(0, 0, 20, 4);
    let mut tree = WidgetTree::new(viewport, 1).expect("tree");
    tree.push(
        WidgetSpec::new(
            NodeKey::new("status").expect("key"),
            WidgetKind::Status,
            Rect::new(0, 0, 20, 1),
            "ready",
        )
        .expect("widget"),
    )
    .expect("status");
    tree.push(
        WidgetSpec::new(
            NodeKey::new("save").expect("key"),
            WidgetKind::Button,
            Rect::new(0, 1, 8, 1),
            "save",
        )
        .expect("widget")
        .action(NodeKey::new("save.action").expect("binding")),
    )
    .expect("button");
    assert_eq!(tree.surface().nodes().len(), 2);
    assert_eq!(tree.surface().generation(), 1);
}
