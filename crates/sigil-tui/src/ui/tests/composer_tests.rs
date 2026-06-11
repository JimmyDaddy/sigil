use ratatui::{Terminal, backend::TestBackend};

use crate::timeline::RunPhase;

use super::*;

#[test]
fn composer_input_aligns_with_header_after_gap() -> anyhow::Result<()> {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        reasoning_effort_label: "max".to_owned(),
        input: "/resume".to_owned(),
        input_rows: 1,
        cursor_position: (7, 0),
    };
    let backend = TestBackend::new(72, 5);
    let mut terminal = Terminal::new(backend)?;

    terminal.draw(|frame| render_input(frame, frame.area(), &view_model))?;

    let content = terminal.backend().buffer().content();
    let width = 72usize;
    let rendered = content.iter().map(|cell| cell.symbol()).collect::<String>();
    assert!(rendered.contains("Build"));
    assert!(rendered.contains("deepseek-v4-pro"));
    assert!(rendered.contains("deepseek"));
    assert!(rendered.contains("max"));
    assert!(!rendered.contains("Composer"));
    assert_eq!(content[width + 3].symbol(), "B");
    assert_eq!(content[(2 * width) + 3].symbol(), " ");
    assert_eq!(content[(3 * width) + 3].symbol(), "/");
    Ok(())
}

#[test]
fn composer_cursor_origin_scrolls_with_multiline_input() {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        reasoning_effort_label: "max".to_owned(),
        input: "one\ntwo\nthree\nfour".to_owned(),
        input_rows: 4,
        cursor_position: (2, 3),
    };

    assert_eq!(
        composer_cursor_origin(ratatui::layout::Rect::new(0, 0, 32, 5), &view_model),
        Some((3, 3))
    );
}

#[test]
fn composer_cursor_origin_returns_none_when_input_area_disappears() {
    let view_model = ComposerViewModel {
        mode_label: "Build".to_owned(),
        phase: RunPhase::Idle,
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-pro".to_owned(),
        reasoning_effort_label: "max".to_owned(),
        input: String::new(),
        input_rows: 1,
        cursor_position: (0, 0),
    };

    assert_eq!(
        composer_cursor_origin(ratatui::layout::Rect::new(0, 0, 4, 2), &view_model),
        None
    );
}
