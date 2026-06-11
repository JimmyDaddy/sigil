use super::PaneFocus;

#[test]
fn pane_focus_labels_are_stable() {
    assert_eq!(PaneFocus::Composer.label(), "composer");
    assert_eq!(PaneFocus::Activity.label(), "activity");
}
