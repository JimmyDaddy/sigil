use ratatui::{Terminal, backend::TestBackend};

use crate::timeline::RunPhase;

use super::*;

#[test]
fn composer_input_aligns_with_header_after_gap() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        phase: RunPhase::Idle,
        is_busy: false,
        model_name: "deepseek-v4-pro".to_owned(),
        reasoning_effort_label: "max".to_owned(),
        run_phase_label: "ready".to_owned(),
        input: "/resume".to_owned(),
        input_rows: 1,
        cursor_position: (7, 0),
    };
    let backend = TestBackend::new(48, 5);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;

    let content = terminal.backend().buffer().content();
    let width = 48usize;
    assert_eq!(content[width + 3].symbol(), "C");
    assert_eq!(content[(2 * width) + 3].symbol(), " ");
    assert_eq!(content[(3 * width) + 3].symbol(), "/");
    Ok(())
}
