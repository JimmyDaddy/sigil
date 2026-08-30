use ratatui::{layout::Rect, widgets::Block};

use super::ScratchRenderContext;

#[test]
fn scratch_context_scissors_custom_widgets_to_assigned_bounds() {
    let mut context = ScratchRenderContext::new(Rect::new(4, 3, 5, 2), Rect::new(0, 0, 20, 20));
    assert_eq!(context.bounds(), Rect::new(4, 3, 5, 2));
    assert_eq!(context.scissor(), Rect::new(4, 3, 5, 2));
    assert!(context.render_widget(Rect::new(0, 0, 20, 20), Block::default()));
    assert_eq!(context.buffer().area, Rect::new(4, 3, 5, 2));
    assert!(!context.render_widget(Rect::new(0, 0, 0, 0), Block::default()));
}
