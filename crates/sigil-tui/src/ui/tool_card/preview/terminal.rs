use super::*;

pub(in crate::ui::tool_card) fn render_terminal_task_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let subtitle = summary
        .metadata
        .terminal_command
        .as_deref()
        .map(|command| truncate_inline_text(command, 120))
        .or_else(|| summary.metadata.terminal_log_path.clone())
        .unwrap_or_else(|| "terminal task".to_owned());
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "terminal",
        palette.accent_warning,
        vec![Span::styled(
            subtitle,
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if let Some(log_path) = &summary.metadata.terminal_log_path {
        lines.push(timeline_content_line(
            accent,
            vec![
                section_badge_with_palette("log", palette.accent_secondary, palette),
                Span::raw(" "),
                Span::styled(log_path.clone(), Style::default().fg(palette.text_muted)),
            ],
        ));
    }
    if summary.preview_lines.is_empty() {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "(no output preview)".to_owned(),
                Style::default().fg(palette.text_muted),
            )],
        ));
    } else {
        lines.extend(render_code_preview_lines_with_palette(
            accent,
            &summary.preview_lines,
            palette.markdown_code_bg,
            palette,
        ));
    }
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    lines
}
pub(in crate::ui::tool_card) fn terminal_task_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "terminal_task")
        || summary.metadata.terminal_task_id.is_some()
}

pub(in crate::ui::tool_card) fn terminal_task_is_active(summary: &ToolCardRender) -> bool {
    matches!(
        summary.metadata.terminal_status.as_deref(),
        Some("starting" | "running")
    )
}

pub(in crate::ui::tool_card) fn terminal_task_display_status(
    summary: &ToolCardRender,
) -> ToolCardDisplayStatus {
    let label = match summary.metadata.terminal_status.as_deref() {
        Some("starting") => "STARTING",
        Some("running") => "RUNNING",
        Some("exited") => "EXITED",
        Some("failed") => "FAILED",
        Some("cancelled") => "CANCELLED",
        Some("interrupted") => "INTERRUPTED",
        _ if summary.is_error => "ERROR",
        _ => "OK",
    };
    let mut details = Vec::new();
    match summary.metadata.terminal_status.as_deref() {
        Some("exited") => {
            if let Some(code) = summary.metadata.terminal_exit_code {
                details.push(format!("exit {code}"));
            }
        }
        Some("failed") => {
            if let Some(reason) = summary.metadata.terminal_failed_reason.as_deref() {
                details.push(truncate_inline_text(reason, 80));
            }
        }
        _ => {}
    }
    if let Some(boundary) = terminal_execution_boundary_detail(summary) {
        details.push(boundary);
    }
    if let Some(cleanup_status) = summary
        .metadata
        .terminal_cleanup_status
        .as_deref()
        .filter(|status| *status != "not_needed")
    {
        details.push(format!("cleanup {cleanup_status}"));
    }
    ToolCardDisplayStatus {
        label,
        detail: (!details.is_empty()).then(|| details.join(" · ")),
        kind: terminal_task_status_kind(summary),
        is_error: summary.is_error
            || matches!(summary.metadata.terminal_status.as_deref(), Some("failed")),
    }
}

pub(in crate::ui::tool_card) fn terminal_execution_boundary_detail(
    summary: &ToolCardRender,
) -> Option<String> {
    let backend = summary.metadata.terminal_enforcement_backend.as_deref();
    let profile = summary.metadata.terminal_sandbox_profile.as_deref();
    match (backend, profile) {
        (Some("local"), Some("unconfined")) => Some("local unconfined".to_owned()),
        (Some(backend), Some(profile)) => Some(format!("{backend} {profile}")),
        (Some(backend), None) => Some(backend.to_owned()),
        (None, Some(profile)) => Some(profile.to_owned()),
        (None, None) => None,
    }
}

pub(in crate::ui::tool_card) fn terminal_task_status_kind(summary: &ToolCardRender) -> StatusKind {
    match summary.metadata.terminal_status.as_deref() {
        Some("starting" | "running") => StatusKind::Running,
        Some("exited") => StatusKind::Success,
        Some("failed" | "cancelled" | "interrupted") => StatusKind::Error,
        _ if summary.is_error => StatusKind::Error,
        _ => StatusKind::Success,
    }
}
