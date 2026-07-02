use super::*;

pub(in crate::ui::tool_card) fn agent_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "spawn_agent")
        || tool_name_matches(&summary.tool_name, "wait_agent")
        || tool_name_matches(&summary.tool_name, "read_agent_result")
        || tool_name_matches(&summary.tool_name, "message_agent")
        || tool_name_matches(&summary.tool_name, "close_agent")
}

pub(in crate::ui::tool_card) fn render_agent_tool_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    if tool_name_matches(&summary.tool_name, "read_agent_result") {
        return render_agent_result_page_preview(summary, accent, max_content_width, palette);
    }
    if tool_name_matches(&summary.tool_name, "spawn_agent")
        && summary.preview_kind == ToolPreviewKind::Markdown
        && !summary.preview_lines.is_empty()
    {
        return render_agent_summary_preview(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    }
    render_agent_status_preview(summary, accent, palette)
}

pub(in crate::ui::tool_card) fn render_agent_result_page_preview(
    summary: &ToolCardRender,
    accent: Color,
    _max_content_width: usize,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "result",
        palette.accent_info,
        vec![Span::styled(
            agent_result_page_summary(summary).unwrap_or_else(|| "agent result page".to_owned()),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    lines.extend(render_agent_status_preview(summary, accent, palette));
    lines
}

pub(in crate::ui::tool_card) fn render_agent_summary_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "summary",
        palette.accent_info,
        vec![Span::styled(
            agent_status_detail(summary),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    lines.extend(render_markdown_timeline_lines_with_palette(
        accent,
        Style::default().fg(palette.text_primary),
        &summary.preview_lines.join("\n"),
        MarkdownRenderOptions::tool_preview(max_content_width).with_syntax_theme(syntax_theme),
        palette,
    ));
    lines.extend(render_tool_hidden_tail(
        accent,
        summary.hidden_lines,
        palette,
    ));
    if agent_payload_bool(summary, "summary_truncated").unwrap_or(false) {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                "Use read_agent_result for the complete result.",
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    lines
}

pub(in crate::ui::tool_card) fn render_agent_status_preview(
    summary: &ToolCardRender,
    accent: Color,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let mut details = vec![Span::styled(
        agent_status_detail(summary),
        Style::default().fg(palette.text_muted),
    )];
    if agent_payload_bool(summary, "result_available").unwrap_or(false) {
        details.push(Span::raw(" · "));
        details.push(Span::styled(
            "result ready",
            Style::default()
                .fg(palette.accent_success)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        "agent",
        palette.accent_info,
        details,
        palette,
    )];
    if let Some(reason) =
        agent_payload_string(summary, "reason").filter(|reason| !reason.is_empty())
    {
        lines.push(timeline_content_line(
            accent,
            vec![Span::styled(
                reason,
                Style::default()
                    .fg(palette.text_muted)
                    .add_modifier(Modifier::ITALIC),
            )],
        ));
    }
    if let Some(action_hint) = agent_payload_string(summary, "action_hint")
        .or_else(|| agent_payload_string(summary, "next_action"))
        .filter(|hint| !hint.is_empty())
    {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled("action", Style::default().fg(palette.text_muted)),
                Span::raw(" "),
                Span::styled(
                    action_hint,
                    Style::default()
                        .fg(palette.accent_warning)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
        ));
    }
    if agent_result_read_tool(summary).is_some() {
        lines.push(timeline_content_line(
            accent,
            vec![
                Span::styled("read", Style::default().fg(palette.text_muted)),
                Span::raw(" "),
                Span::styled(
                    "read_agent_result",
                    Style::default().fg(palette.accent_info),
                ),
            ],
        ));
    }
    lines
}

pub(in crate::ui::tool_card) fn agent_tool_display_status(status: &str) -> ToolCardDisplayStatus {
    let status = status.trim();
    let kind = status_kind_from_label(status);
    ToolCardDisplayStatus {
        label: agent_status_display_label(status),
        detail: None,
        kind,
        is_error: kind == StatusKind::Error,
    }
}

pub(in crate::ui::tool_card) fn agent_tool_display_summary(
    summary: &ToolCardRender,
) -> Option<String> {
    if tool_name_matches(&summary.tool_name, "read_agent_result") {
        return agent_result_page_summary(summary);
    }
    if tool_name_matches(&summary.tool_name, "wait_agent") {
        if agent_payload_bool(summary, "result_available").unwrap_or(false) {
            return Some("result ready".to_owned());
        }
        return Some("result pending".to_owned());
    }
    if tool_name_matches(&summary.tool_name, "spawn_agent")
        && agent_payload_bool(summary, "summary_truncated").unwrap_or(false)
    {
        return Some("summary truncated · read_agent_result available".to_owned());
    }
    if tool_name_matches(&summary.tool_name, "spawn_agent")
        && !agent_payload_bool(summary, "result_available").unwrap_or(false)
    {
        return Some("result pending".to_owned());
    }
    None
}

pub(in crate::ui::tool_card) fn agent_tool_title(summary: &ToolCardRender) -> ToolCardTitle {
    let thread = agent_thread_label(summary);
    if tool_name_matches(&summary.tool_name, "spawn_agent") {
        if thread == "agent" {
            return ToolCardTitle::new("Started", "agent", None);
        }
        return ToolCardTitle::new("Started", "agent", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "wait_agent") {
        return ToolCardTitle::new("Checked", "agent", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "read_agent_result") {
        return ToolCardTitle::new("Read", "agent result", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "message_agent") {
        return ToolCardTitle::new("Messaged", "agent", Some(thread));
    }
    if tool_name_matches(&summary.tool_name, "close_agent") {
        return ToolCardTitle::new("Closed", "agent", Some(thread));
    }
    ToolCardTitle::new("Called", "agent", Some(thread))
}

pub(in crate::ui::tool_card) fn agent_status_display_label(status: &str) -> &'static str {
    match status {
        "idle" => "IDLE",
        "started" => "STARTED",
        "running" => "RUNNING",
        "completed" => "DONE",
        "failed" => "FAILED",
        "blocked" => "BLOCKED",
        "cancelled" => "CANCELLED",
        "interrupted" => "INTERRUPTED",
        "closed" => "CLOSED",
        "unavailable" => "UNAVAILABLE",
        "unknown" => "UNKNOWN",
        _ => "AGENT",
    }
}

pub(in crate::ui::tool_card) fn agent_status_detail(summary: &ToolCardRender) -> String {
    let status = agent_payload_string(summary, "status").unwrap_or_else(|| "unknown".to_owned());
    format!("{} · {}", status, agent_thread_label(summary))
}

pub(in crate::ui::tool_card) fn agent_result_page_summary(
    summary: &ToolCardRender,
) -> Option<String> {
    if agent_payload_bool(summary, "already_delivered").unwrap_or(false) {
        return Some("already delivered · rerun not needed".to_owned());
    }
    let page = agent_payload_value(summary)?.get("page")?;
    let offset = page.get("offset_chars").and_then(Value::as_u64)?;
    let returned = page.get("returned_chars").and_then(Value::as_u64)?;
    let total = page.get("total_chars").and_then(Value::as_u64)?;
    let more = if page
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        " · more"
    } else {
        ""
    };
    Some(format!("chars {offset}+{returned}/{total}{more}"))
}

pub(in crate::ui::tool_card) fn agent_result_read_tool(summary: &ToolCardRender) -> Option<String> {
    let value = agent_payload_value(summary)?;
    value
        .get("result_ref")
        .and_then(|result_ref| result_ref.get("read_tool"))
        .or_else(|| {
            value
                .get("result_fetch")
                .and_then(|result_fetch| result_fetch.get("tool"))
        })
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(in crate::ui::tool_card) fn agent_thread_label(summary: &ToolCardRender) -> String {
    let display_name = agent_payload_string(summary, "display_name");
    let objective = agent_payload_string(summary, "objective");
    let profile_id = agent_payload_string(summary, "profile_id")
        .or_else(|| call_argument(summary, "profile_id"));
    let thread_id =
        agent_payload_string(summary, "thread_id").or_else(|| call_argument(summary, "thread_id"));
    truncate_inline_text(
        &resolve_agent_display_name(AgentDisplayNameInput {
            display_name: display_name.as_deref(),
            objective: objective.as_deref(),
            profile_id: profile_id.as_deref(),
            thread_id: thread_id.as_deref(),
            ..AgentDisplayNameInput::default()
        })
        .label,
        48,
    )
}

pub(in crate::ui::tool_card) fn agent_payload_value(summary: &ToolCardRender) -> Option<&Value> {
    summary.preview_value.as_ref()
}

pub(in crate::ui::tool_card) fn agent_payload_string(
    summary: &ToolCardRender,
    key: &str,
) -> Option<String> {
    let value = agent_payload_value(summary)?.get(key)?;
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Null => None,
        _ => Some(value.to_string()),
    }
}

pub(in crate::ui::tool_card) fn agent_payload_bool(
    summary: &ToolCardRender,
    key: &str,
) -> Option<bool> {
    agent_payload_value(summary)?.get(key)?.as_bool()
}
