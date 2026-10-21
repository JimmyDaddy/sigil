use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn view(phase: CheckpointRestoreModalPhase) -> CheckpointRestoreModalView {
    CheckpointRestoreModalView {
        phase,
        phase_detail: "No preview is available".to_owned(),
        summary_lines: Vec::new(),
        body_title: "Restore evidence",
        body_status: String::new(),
        body_notice_lines: Vec::new(),
        body_lines: vec!["No recorded reverse diff is available.".to_owned()],
        body_is_diff: false,
        scroll: 0,
        can_restore: false,
        can_fork: false,
        error: None,
    }
}

#[test]
fn compact_checkpoint_empty_uses_one_short_border_and_only_refresh_close() -> anyhow::Result<()> {
    let state = view(CheckpointRestoreModalPhase::Empty);
    let screen = Rect::new(0, 0, 80, 24);
    let layout = checkpoint_restore_layout(screen, &state);
    assert!(layout.area.height <= 8);
    assert_eq!(layout.target, Rect::default());
    assert_eq!(checkpoint_restore_max_scroll(80, 24, &state), 0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| {
        render_checkpoint_restore_modal_view(frame, &state, &theme::Theme::default())
    })?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("No controlled checkpoint yet"));
    assert!(text.contains("Ctrl-R refresh · Esc close"));
    assert!(!text.contains("Restore target"));
    assert!(!text.contains("Restore evidence"));
    assert_eq!(
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "╭")
            .count(),
        1
    );
    Ok(())
}

#[test]
fn compact_checkpoint_unavailable_keeps_full_error_target_evidence_and_tail_reachable()
-> anyhow::Result<()> {
    let mut state = view(CheckpointRestoreModalPhase::Unavailable);
    state.error = Some(format!(
        "ERROR_HEAD\n\n{}\nERROR_TAIL\n",
        "完整错误 e\u{301}🙂 ".repeat(500)
    ));
    state.summary_lines = vec!["Target: before turn 7\n\nTARGET_CONTINUATION".to_owned()];
    state.body_notice_lines =
        vec!["Blocked: src/lib.rs · changed since preview\nBLOCKED_CONTINUATION".to_owned()];
    state.body_lines =
        vec!["EVIDENCE_HEAD\n\nEVIDENCE_TAIL: current_hash -> target_hash".to_owned()];
    let screen = Rect::new(0, 0, 80, 24);
    let layout = checkpoint_restore_layout(screen, &state);
    let rows = compact_status_rows(&state, layout.body_content.width as usize);
    let source = rows.iter().map(ToString::to_string).collect::<String>();
    let plain = rows.iter().map(ToString::to_string).collect::<Vec<_>>();
    assert_eq!(
        &plain[..4],
        ["Checkpoint unavailable", "", "ERROR_HEAD", ""]
    );
    assert!(plain.iter().any(|line| line == "ERROR_TAIL"));
    assert!(
        plain
            .windows(3)
            .any(|rows| rows == ["Target: before turn 7", "", "TARGET_CONTINUATION"])
    );
    assert!(plain.windows(2).any(|rows| rows
        == [
            "Blocked: src/lib.rs · changed since preview",
            "BLOCKED_CONTINUATION"
        ]));
    assert!(plain.windows(3).any(|rows| rows
        == [
            "EVIDENCE_HEAD",
            "",
            "EVIDENCE_TAIL: current_hash -> target_hash"
        ]));

    for fact in [
        "ERROR_HEAD",
        "ERROR_TAIL",
        "Target: before turn 7",
        "Blocked: src/lib.rs",
        "EVIDENCE_TAIL",
    ] {
        assert!(source.contains(fact));
    }
    let max = checkpoint_restore_max_scroll(80, 24, &state);
    assert_eq!(max, rows.len() - layout.body_content.height as usize);
    assert!(max > layout.body_content.height as usize);
    state.scroll = max as u16;
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    terminal.draw(|frame| {
        render_checkpoint_restore_modal_view(frame, &state, &theme::Theme::default())
    })?;
    let text = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("EVIDENCE_TAIL"));
    assert!(text.contains("Esc close"));
    assert!(!text.contains("Enter restore"));
    assert_eq!(layout.actions.bottom(), layout.area.bottom() - 1);
    Ok(())
}
