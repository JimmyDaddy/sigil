use ratatui::{Terminal, backend::TestBackend, layout::Rect};

use super::{
    IntentStackModalPhase, IntentStackModalView, intent_stack_modal_layout,
    render_intent_stack_modal_view,
};
use crate::app::IntentStackRowView;
use crate::ui::theme;

fn view(row_count: usize) -> IntentStackModalView {
    IntentStackModalView {
        phase: IntentStackModalPhase::Ready,
        phase_detail: "review".to_owned(),
        summary_lines: vec!["summary".to_owned()],
        rows: (0..row_count)
            .map(|index| IntentStackRowView {
                title: format!("Intent {index}"),
                status: "applied",
                detail: "1 artifact".to_owned(),
            })
            .collect(),
        selected: row_count.saturating_sub(1),
        detail_lines: vec!["detail".to_owned()],
        detail_scroll: 0,
        primary_action_label: "Preview Drop".to_owned(),
        primary_action_enabled: true,
        can_refresh: true,
        error: None,
    }
}

#[test]
fn narrow_layout_stacks_list_and_detail_without_leaving_the_modal() {
    let screen = Rect::new(0, 0, 46, 22);
    let layout = intent_stack_modal_layout(screen, &view(3));

    assert!(layout.area.width <= screen.width);
    assert!(layout.area.height <= screen.height);
    assert!(layout.list.y < layout.detail.y);
    assert!(layout.detail.bottom() <= layout.actions.y);
    assert_eq!(layout.row_areas.len(), 3);
    assert!(layout.primary_action.right() <= layout.actions.right());
}

#[test]
fn list_window_keeps_the_selected_intent_mouse_target_visible() {
    let screen = Rect::new(0, 0, 90, 18);
    let layout = intent_stack_modal_layout(screen, &view(20));

    assert!(layout.row_areas.iter().any(|(index, _)| *index == 19));
    assert!(
        layout
            .row_areas
            .iter()
            .all(|(_, area)| area.x >= layout.list.x && area.right() <= layout.list.right())
    );
}

#[test]
fn tiny_layout_never_exceeds_the_terminal() {
    let screen = Rect::new(0, 0, 18, 7);
    let layout = intent_stack_modal_layout(screen, &view(1));

    assert!(layout.area.width <= screen.width);
    assert!(layout.area.height <= screen.height);
    assert!(layout.primary_action.right() <= screen.right());
    assert!(layout.actions.bottom() <= screen.bottom());
}

#[test]
fn rendered_review_keeps_list_detail_and_single_action_distinct() -> anyhow::Result<()> {
    let mut review = view(2);
    review.detail_lines = vec![
        "Retained artifacts".to_owned(),
        "src/lib.rs · shared · available".to_owned(),
        "Blocked / manual resolution required".to_owned(),
    ];
    let mut terminal = Terminal::new(TestBackend::new(96, 26))?;
    let palette = theme::default_palette();

    terminal.draw(|frame| render_intent_stack_modal_view(frame, &review, &palette))?;

    let rendered = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(rendered.contains("Intent Stack"));
    assert!(rendered.contains("Changes (2)"));
    assert!(rendered.contains("Retained artifacts"));
    assert!(rendered.contains("manual resolution required"));
    assert_eq!(rendered.matches("Preview Drop").count(), 1);
    Ok(())
}
