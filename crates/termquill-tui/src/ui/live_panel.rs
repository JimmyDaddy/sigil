use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::view_model::{LiveActivityViewModel, LivePanelViewModel};

use super::{
    geometry::inset_rect,
    theme::{ink, phase_accent, shell_bg},
};

const LIVE_PANEL_BOTTOM_PADDING: u16 = 1;

pub(crate) fn render_live_panel(frame: &mut Frame, area: Rect, view_model: &LivePanelViewModel) {
    frame.render_widget(
        Block::default().style(Style::default().bg(shell_bg())),
        area,
    );
    if area.width == 0 || area.height == 0 {
        return;
    }

    let inner = inset_rect(area, 1, 0);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let content_frame = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner
            .height
            .saturating_sub(LIVE_PANEL_BOTTOM_PADDING)
            .max(1),
    );

    let activity = view_model
        .activity
        .as_ref()
        .map(|activity| render_live_activity_line(activity, phase_accent(&view_model.phase)));
    let mut lines = view_model.transcript_lines.clone();
    if let Some(activity_line) = activity {
        lines.push(activity_line);
    }
    let visual_lines = wrap_live_panel_lines(lines, content_frame.width as usize);
    let visual_start = visual_lines
        .len()
        .saturating_sub(content_frame.height as usize);
    let visual_lines = visual_lines
        .into_iter()
        .skip(visual_start)
        .collect::<Vec<_>>();
    let content_height = visual_lines.len().max(1) as u16;
    let content_y = content_frame
        .y
        .saturating_add(content_frame.height.saturating_sub(content_height));
    let content_area = Rect::new(
        content_frame.x,
        content_y,
        content_frame.width,
        content_height,
    );
    frame.render_widget(
        Paragraph::new(Text::from(visual_lines))
            .style(Style::default().bg(shell_bg()))
            .wrap(Wrap { trim: false }),
        content_area,
    );
}

fn wrap_live_panel_lines(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in lines {
        rows.extend(wrap_live_panel_line(line, width));
    }
    if rows.is_empty() {
        rows.push(Line::raw(String::new()));
    }
    rows
}

fn wrap_live_panel_line(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut current_spans = Vec::new();
    let mut current_width = 0usize;
    let line_style = line.style;
    let line_alignment = line.alignment;

    for span in line.spans {
        let mut segment = String::new();
        for grapheme in span.content.as_ref().graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
            if current_width > 0 && current_width + grapheme_width > width {
                push_live_panel_segment(&mut current_spans, &mut segment, span.style);
                rows.push(live_panel_wrapped_line(
                    std::mem::take(&mut current_spans),
                    line_style,
                    line_alignment,
                ));
                current_width = 0;
            }
            segment.push_str(grapheme);
            current_width += grapheme_width;
        }
        push_live_panel_segment(&mut current_spans, &mut segment, span.style);
    }

    rows.push(live_panel_wrapped_line(
        current_spans,
        line_style,
        line_alignment,
    ));
    rows
}

fn push_live_panel_segment(spans: &mut Vec<Span<'static>>, segment: &mut String, style: Style) {
    if segment.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(segment), style));
}

fn live_panel_wrapped_line(
    spans: Vec<Span<'static>>,
    style: Style,
    alignment: Option<ratatui::layout::Alignment>,
) -> Line<'static> {
    Line {
        spans,
        style,
        alignment,
    }
}

pub(crate) fn render_live_activity_line(
    summary: &LiveActivityViewModel,
    accent: Color,
) -> Line<'static> {
    let spinner = live_spinner_frame();
    Line::from(vec![
        Span::styled(
            format!("{spinner} {}", summary.label),
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(summary.detail.clone(), Style::default().fg(ink())),
    ])
}

pub(crate) fn live_spinner_frame() -> &'static str {
    const FRAMES: &[&str] = &["◴", "◷", "◶", "◵"];
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() / 120)
        .unwrap_or(0);
    FRAMES[(tick as usize) % FRAMES.len()]
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::Path};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend};
    use termquill_kernel::{
        AgentConfig, CompactionConfig, MemoryConfig, PermissionConfig, RootConfig, SessionConfig,
        WorkspaceConfig,
    };

    use crate::{app::AppState, view_model::LivePanelViewModel};

    use super::*;
    use crate::ui::theme::phase_accent;

    fn test_config() -> RootConfig {
        RootConfig {
            workspace: WorkspaceConfig {
                root: ".".to_owned(),
            },
            session: SessionConfig {
                log_dir: ".termquill/sessions".to_owned(),
            },
            agent: AgentConfig {
                provider: "deepseek".to_owned(),
                model: "deepseek-v4-flash".to_owned(),
                max_turns: 8,
                tool_timeout_secs: 30,
            },
            permission: PermissionConfig::default(),
            memory: MemoryConfig { enabled: true },
            compaction: CompactionConfig::default(),
            providers: BTreeMap::new(),
            mcp_servers: Vec::new(),
        }
    }

    #[test]
    fn render_live_activity_line_shows_current_phase() -> anyhow::Result<()> {
        let mut app = AppState::from_root_config(Path::new("/tmp/termquill.toml"), &test_config());
        app.set_terminal_size(120, 30);
        app.handle_key_event(KeyEvent::new(KeyCode::Char('你'), KeyModifiers::NONE))?;
        app.handle_key_event(KeyEvent::new(KeyCode::Char('好'), KeyModifiers::NONE))?;
        let _ = app.submit_input()?;

        let view_model = LivePanelViewModel::from_app(&app, 4);
        let line = render_live_activity_line(
            view_model
                .activity
                .as_ref()
                .expect("busy run should expose live activity"),
            phase_accent(&view_model.phase),
        );
        let plain = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();

        assert!(plain.contains("thinking"));
        assert!(plain.contains("reasoning with"));
        Ok(())
    }

    #[test]
    fn render_live_panel_keeps_wrapped_tail_visible() -> anyhow::Result<()> {
        let view_model = LivePanelViewModel {
            phase: crate::timeline::RunPhase::Idle,
            activity: None,
            transcript_lines: vec![Line::from(
                "prefix words that wrap across rows before visible TAIL",
            )],
        };
        let backend = TestBackend::new(16, 4);
        let mut terminal = Terminal::new(backend)?;

        terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("TAIL"));
        Ok(())
    }

    #[test]
    fn render_live_panel_keeps_bottom_padding_clear() -> anyhow::Result<()> {
        let view_model = LivePanelViewModel {
            phase: crate::timeline::RunPhase::Idle,
            activity: None,
            transcript_lines: vec![Line::from("visible tail")],
        };
        let backend = TestBackend::new(16, 4);
        let mut terminal = Terminal::new(backend)?;

        terminal.draw(|frame| render_live_panel(frame, frame.area(), &view_model))?;

        let buffer = terminal.backend().buffer();
        let bottom_row = buffer.content()[48..64]
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(bottom_row.trim().is_empty());
        Ok(())
    }
}
