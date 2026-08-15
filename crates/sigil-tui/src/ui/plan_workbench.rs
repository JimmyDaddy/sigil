use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Paragraph},
};

use crate::app::{PendingPlanApproval, PlanWorkbenchAction};

use super::{
    text::wrap_terminal_lines,
    theme::{Theme, styles},
};

pub(super) fn render_plan_workbench(
    frame: &mut Frame,
    area: Rect,
    pending: &PendingPlanApproval,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(area);
    render_header(frame, rows[0], pending, theme);
    render_body(frame, rows[1], pending, theme);
    render_actions(frame, rows[2], pending, theme);
}

fn render_header(frame: &mut Frame, area: Rect, pending: &PendingPlanApproval, theme: &Theme) {
    let stale = if pending.stale { " · stale" } else { "" };
    let mut title_spans = vec![
        Span::styled(
            "Plan Review",
            Style::default()
                .fg(theme.palette.accent_warning)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · {} steps{stale}", pending.detail.steps.len()),
            styles::muted(&theme.palette),
        ),
    ];
    if let Some(revision) = pending.revision.as_ref() {
        let status = match revision.status {
            sigil_kernel::PublicPlanRevisionStatusV1::AwaitingGuidance => "awaiting guidance",
            sigil_kernel::PublicPlanRevisionStatusV1::Queued => "queued",
            sigil_kernel::PublicPlanRevisionStatusV1::Researching => "researching",
            sigil_kernel::PublicPlanRevisionStatusV1::WaitingForInput => "waiting for input",
            sigil_kernel::PublicPlanRevisionStatusV1::Finalizing => "finalizing",
            sigil_kernel::PublicPlanRevisionStatusV1::Failed => "failed; original restored",
            sigil_kernel::PublicPlanRevisionStatusV1::Cancelled => "cancelled; original restored",
            sigil_kernel::PublicPlanRevisionStatusV1::Succeeded => "succeeded",
        };
        title_spans.push(Span::styled(
            format!(" · revision {status}"),
            styles::muted(&theme.palette),
        ));
    }
    let title = Line::from(title_spans);
    let hint = Line::from(Span::styled(
        "↑↓/Pg scroll · Tab/←→ action · Enter confirm · Esc close",
        styles::muted(&theme.palette),
    ));
    frame.render_widget(
        Paragraph::new(Text::from(vec![title, hint])).style(styles::body(&theme.palette)),
        area,
    );
}

fn render_body(frame: &mut Frame, area: Rect, pending: &PendingPlanApproval, theme: &Theme) {
    let inner = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(theme.palette.border_subtle))
        .inner(area);
    frame.render_widget(
        Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(Style::default().fg(theme.palette.border_subtle))
            .style(styles::body(&theme.palette)),
        area,
    );
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut lines = plan_detail_lines(pending, theme);
    lines = wrap_terminal_lines(lines, inner.width as usize);
    let max_scroll = lines.len().saturating_sub(inner.height as usize);
    pending.workbench_scroll_extent.set(max_scroll);
    let scroll = pending.workbench_scroll.min(max_scroll);
    let visible = lines
        .into_iter()
        .skip(scroll)
        .take(inner.height as usize)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(visible)).style(styles::body(&theme.palette)),
        inner,
    );
}

fn plan_detail_lines(pending: &PendingPlanApproval, theme: &Theme) -> Vec<Line<'static>> {
    let detail = &pending.detail;
    let mut lines = vec![
        heading("Summary", theme),
        Line::raw(detail.summary.clone()),
        Line::raw(String::new()),
        heading("Steps", theme),
    ];
    for (index, step) in detail.steps.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{}. ", index + 1),
                Style::default().fg(theme.palette.accent_primary),
            ),
            Span::styled(
                step.title.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(text) = &step.detail {
            lines.push(indented("   ", text));
        }
        let contract = [
            step.role.map(|value| format!("role={}", value.as_str())),
            step.mode.map(|value| format!("mode={}", value.as_str())),
            step.isolation
                .map(|value| format!("isolation={}", value.as_str())),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        if !contract.is_empty() {
            lines.push(indented("   ", &contract));
        }
        push_list(&mut lines, "   depends on: ", &step.depends_on);
        push_list(&mut lines, "   paths: ", &step.target_paths);
        let checks = step
            .suggested_checks
            .iter()
            .map(render_check)
            .collect::<Vec<_>>();
        push_list(&mut lines, "   checks: ", &checks);
        if let Some(risk) = &step.risk {
            lines.push(indented("   risk: ", risk));
        }
        push_list(&mut lines, "   notes: ", &step.notes);
        lines.push(Line::raw(String::new()));
    }
    if !detail.target_paths.is_empty() {
        lines.push(heading("Scope", theme));
        for path in &detail.target_paths {
            lines.push(indented("• ", path));
        }
        lines.push(Line::raw(String::new()));
    }
    if !detail.suggested_checks.is_empty() {
        lines.push(heading("Verification", theme));
        for check in &detail.suggested_checks {
            lines.push(indented("• ", &render_check(check)));
        }
        lines.push(Line::raw(String::new()));
    }
    if let Some(risk) = &detail.risk {
        lines.push(heading("Risk", theme));
        lines.push(Line::raw(risk.clone()));
        lines.push(Line::raw(String::new()));
    }
    if !detail.notes.is_empty() {
        lines.push(heading("Notes", theme));
        for note in &detail.notes {
            lines.push(indented("• ", note));
        }
        lines.push(Line::raw(String::new()));
    }
    lines.push(Line::from(Span::styled(
        format!("plan {} · {}", detail.plan_id.as_str(), detail.plan_hash),
        styles::muted(&theme.palette),
    )));
    lines
}

fn heading(value: &str, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(
        value.to_owned(),
        Style::default()
            .fg(theme.palette.accent_info)
            .add_modifier(Modifier::BOLD),
    ))
}

fn indented(prefix: &str, value: &str) -> Line<'static> {
    Line::raw(format!("{prefix}{value}"))
}

fn push_list(lines: &mut Vec<Line<'static>>, prefix: &str, values: &[String]) {
    if !values.is_empty() {
        lines.push(indented(prefix, &values.join(", ")));
    }
}

fn render_check(check: &sigil_kernel::PlanSuggestedCheck) -> String {
    let command = std::iter::once(check.command.command.as_str())
        .chain(check.command.args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let mut metadata = vec![format!("effect={}", check.effect.as_str())];
    if let Some(cwd) = check.command.cwd.as_deref() {
        metadata.push(format!("cwd={}", cwd.display()));
    }
    if let Some(source_line) = check.source_line.as_deref() {
        metadata.push(format!("source={source_line}"));
    }
    format!("{command} [{}]", metadata.join(" · "))
}

fn render_actions(frame: &mut Frame, area: Rect, pending: &PendingPlanApproval, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let spans = PlanWorkbenchAction::ORDER
        .iter()
        .filter(|action| pending.action_allowed(**action))
        .flat_map(|action| {
            let selected = *action == pending.selected_action;
            let disabled = pending.stale
                && matches!(action, PlanWorkbenchAction::Run | PlanWorkbenchAction::Save);
            let style = if selected {
                Style::default()
                    .fg(theme.palette.button_selected_fg)
                    .bg(if disabled {
                        theme.palette.text_disabled
                    } else {
                        theme.palette.button_selected_bg
                    })
                    .add_modifier(Modifier::BOLD)
            } else if disabled {
                Style::default().fg(theme.palette.text_disabled)
            } else {
                Style::default().fg(theme.palette.button_inactive_fg)
            };
            [Span::raw("  "), Span::styled(action.label(), style)]
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(styles::body(&theme.palette)),
        area,
    );
}
