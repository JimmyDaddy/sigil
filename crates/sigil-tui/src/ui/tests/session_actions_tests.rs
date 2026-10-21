use super::*;
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn session_details_wrap_full_identity_and_keep_return_fixed_at_eighty_columns() -> anyhow::Result<()>
{
    let title = format!(
        "TITLE_FIRST\n{}\nTITLE_MIDDLE\n{}\nTITLE_LAST",
        "中文e\u{301}🙂".repeat(100),
        "完整标题".repeat(100)
    );
    let path = format!(
        "/PATH_FIRST/{}/PATH_MIDDLE/{}/PATH_LAST.jsonl",
        "workspace-e\u{301}🙂/".repeat(40),
        "session-record/".repeat(40)
    );
    let lines = vec![
        format!("title: {title}"),
        format!("file: {path}"),
        "UUID: exact-session".to_owned(),
    ];
    let mut view = SessionActionsSurface {
        scroll_rows: 0,
        details_open: true,
    };
    let screen = Rect::new(0, 0, 80, 24);
    let layout = session_actions_layout(screen, &lines, &[], &view);
    let full = layout
        .rows
        .iter()
        .flat_map(|line| line.spans.iter().map(|span| span.content.as_ref()))
        .collect::<String>();
    assert!(full.contains(&title.replace('\n', "")));
    assert!(full.contains(&path));
    assert!(layout.rows.iter().any(|line| {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            == "TITLE_MIDDLE"
    }));
    assert!(layout.max_scroll > layout.body.height as usize);
    let visual = super::super::modal_visual_for(
        Some("Session Actions"),
        false,
        &crate::ui::theme::Theme::default(),
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    let mut seen = String::new();
    let mut fixed_back = None;
    for offset in 0..=layout.max_scroll {
        view.scroll_rows = offset;
        let current = session_actions_layout(screen, &lines, &[], &view);
        assert_eq!(current.actions.len(), 1);
        assert_eq!(current.actions[0].0, SessionModalAction::Back);
        if let Some(expected) = fixed_back {
            assert_eq!(current.actions[0].1, expected);
        }
        fixed_back = Some(current.actions[0].1);
        render_with_view(&mut terminal, &lines, &view, &visual)?;
        let text = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("[back] Session actions"));
        for y in current.body.y..current.body.bottom() {
            let mut row = String::new();
            let mut x = current.body.x;
            while x < current.body.right() {
                let symbol = terminal
                    .backend()
                    .buffer()
                    .cell((x, y))
                    .expect("body cell")
                    .symbol();
                row.push_str(symbol);
                x += crate::ui::terminal_cell_width(symbol).max(1) as u16;
            }
            seen.push_str(row.trim_end());
        }
    }
    for marker in [
        "TITLE_FIRST",
        "TITLE_MIDDLE",
        "TITLE_LAST",
        "PATH_FIRST",
        "PATH_MIDDLE",
        "PATH_LAST.jsonl",
    ] {
        assert!(
            seen.contains(marker),
            "missing visible identity segment {marker}"
        );
    }
    view.scroll_rows = usize::MAX;
    let clamped = session_actions_layout(screen, &lines, &[], &view);
    assert_eq!(clamped.scroll, clamped.max_scroll);
    Ok(())
}

fn render_with_view(
    terminal: &mut Terminal<TestBackend>,
    lines: &[String],
    view: &SessionActionsSurface,
    visual: &ModalVisual,
) -> anyhow::Result<()> {
    terminal.draw(|frame| render_session_actions(frame, lines, &[], view, visual))?;
    Ok(())
}

#[test]
fn session_action_mouse_rows_follow_the_same_wrapped_visual_rows() {
    let label = "[Enter resume] Continue this selected session".to_owned();
    let lines = vec![
        format!("title: {}", "中文e\u{301}🙂".repeat(9)),
        label.clone(),
    ];
    let view = SessionActionsSurface {
        details_open: false,
        scroll_rows: 0,
    };
    let screen = Rect::new(0, 0, 46, 16);
    let actions = vec![(SessionModalAction::Resume, label.clone())];
    let first = session_actions_layout(screen, &lines, &actions, &view);
    let hit = first
        .actions
        .iter()
        .find(|(action, _)| *action == SessionModalAction::Resume)
        .expect("visible resume");
    let physical_index = hit.1.y.saturating_sub(first.body.y) as usize;
    let text = first.rows[physical_index]
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    assert!(text.starts_with("[Enter resume]"));
    let details = session_actions_layout(
        screen,
        &lines,
        &actions,
        &SessionActionsSurface {
            details_open: true,
            scroll_rows: 0,
        },
    );
    assert!(
        details
            .actions
            .iter()
            .all(|(action, _)| *action == SessionModalAction::Back)
    );
}
