use super::*;

#[cfg(test)]
pub(in crate::ui::tool_card) fn render_code_intelligence_preview(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
) -> Vec<Line<'static>> {
    let palette = crate::ui::theme::default_palette();
    render_code_intelligence_preview_with_palette(
        summary,
        accent,
        max_content_width,
        SyntaxThemeId::default(),
        &palette,
    )
}

pub(in crate::ui::tool_card) fn render_code_intelligence_preview_with_palette(
    summary: &ToolCardRender,
    accent: Color,
    max_content_width: usize,
    syntax_theme: SyntaxThemeId,
    palette: &ThemePalette,
) -> Vec<Line<'static>> {
    let Some(value) = &summary.preview_value else {
        return render_generic_tool_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        );
    };
    let server = value
        .get("server")
        .and_then(Value::as_str)
        .or(summary.metadata.code_server.as_deref())
        .unwrap_or("code");
    let capability = value
        .get("capability")
        .and_then(Value::as_str)
        .or(summary.metadata.code_capability.as_deref())
        .unwrap_or("inspect");
    let returned = value
        .get("metadata")
        .and_then(|metadata| metadata.get("returned"))
        .and_then(Value::as_u64)
        .or(summary.metadata.returned_entries)
        .unwrap_or(0);
    let total = value
        .get("metadata")
        .and_then(|metadata| metadata.get("total"))
        .and_then(Value::as_u64)
        .or(summary.metadata.total_entries)
        .unwrap_or(returned);
    let mut lines = vec![timeline_section_line_with_palette(
        accent,
        code_intelligence_section(summary),
        palette.accent_info,
        vec![Span::styled(
            format!(
                "{} · {} · {} · {returned}/{total}",
                code_intelligence_source_label(server, capability),
                server,
                code_intelligence_capability_label(capability)
            ),
            Style::default().fg(palette.text_muted),
        )],
        palette,
    )];
    if let Some(server_line) = code_intelligence_servers_line_with_palette(value, palette) {
        lines.push(timeline_content_line(accent, server_line));
    }
    if let Some(items) = code_intelligence_items(summary, value, palette) {
        for item in items.into_iter().take(16) {
            lines.push(timeline_content_line(accent, item));
        }
        let hidden = total
            .saturating_sub(returned)
            .saturating_add(returned.saturating_sub(16));
        lines.extend(render_tool_hidden_tail(accent, hidden as usize, palette));
    } else {
        lines.extend(render_generic_tool_preview_with_palette(
            summary,
            accent,
            max_content_width,
            syntax_theme,
            palette,
        ));
    }
    lines
}

pub(in crate::ui::tool_card) fn code_intelligence_section(
    summary: &ToolCardRender,
) -> &'static str {
    if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        "diagnostics"
    } else if tool_name_matches(&summary.tool_name, "code_definition") {
        "definition"
    } else if tool_name_matches(&summary.tool_name, "code_references") {
        "references"
    } else if tool_name_matches(&summary.tool_name, "code_actions") {
        "actions"
    } else {
        "symbols"
    }
}

pub(in crate::ui::tool_card) fn code_intelligence_items(
    summary: &ToolCardRender,
    value: &Value,
    palette: &ThemePalette,
) -> Option<Vec<Vec<Span<'static>>>> {
    let key = if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        "diagnostics"
    } else if tool_name_matches(&summary.tool_name, "code_definition") {
        "definition"
    } else if tool_name_matches(&summary.tool_name, "code_references") {
        "references"
    } else if tool_name_matches(&summary.tool_name, "code_actions") {
        "code_actions"
    } else if tool_name_matches(&summary.tool_name, "code_workspace_symbols") {
        "workspace_symbols"
    } else {
        "symbols"
    };
    let array = value
        .get(key)
        .or_else(|| value.get("results"))
        .and_then(Value::as_array)?;
    let rows = array
        .iter()
        .filter_map(|entry| code_intelligence_row_with_palette(summary, entry, palette))
        .collect::<Vec<_>>();
    Some(rows)
}

pub(in crate::ui::tool_card) fn code_intelligence_row_with_palette(
    summary: &ToolCardRender,
    entry: &Value,
    palette: &ThemePalette,
) -> Option<Vec<Span<'static>>> {
    if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        let severity = entry.get("severity")?.as_str()?.to_owned();
        let path = entry.get("path")?.as_str()?.to_owned();
        let message = entry.get("message")?.as_str()?.to_owned();
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let mut spans = vec![
            section_badge_with_palette(
                &severity,
                diagnostic_severity_color_with_palette(&severity, palette),
                palette,
            ),
            Span::raw(" "),
            Span::styled(
                code_location_label(&path, entry),
                Style::default().fg(palette.accent_info),
            ),
            Span::raw(" "),
        ];
        if let Some(source) = source {
            spans.push(Span::styled(
                format!("{source}: "),
                Style::default().fg(palette.text_muted),
            ));
        }
        spans.push(Span::styled(
            truncate_inline_text(&message, 120),
            Style::default().fg(palette.text_primary),
        ));
        return Some(spans);
    }
    if tool_name_matches(&summary.tool_name, "code_actions") {
        let title = entry.get("title")?.as_str()?.to_owned();
        let label = entry
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("action")
            .to_owned();
        let capability = if entry.get("has_edit").and_then(Value::as_bool) == Some(true) {
            "edit"
        } else if entry.get("has_command").and_then(Value::as_bool) == Some(true) {
            "command"
        } else {
            "inspect"
        };
        return Some(vec![
            section_badge_with_palette(&label, palette.accent_secondary, palette),
            Span::raw(" "),
            Span::styled(capability, Style::default().fg(palette.accent_info)),
            Span::raw(" "),
            Span::styled(
                truncate_inline_text(&title, 120),
                Style::default().fg(palette.text_primary),
            ),
        ]);
    }
    let path = entry.get("path")?.as_str()?.to_owned();
    let label = entry
        .get("kind")
        .and_then(Value::as_str)
        .or_else(|| {
            if tool_name_matches(&summary.tool_name, "code_definition") {
                Some("def")
            } else if tool_name_matches(&summary.tool_name, "code_references") {
                Some("ref")
            } else {
                None
            }
        })
        .unwrap_or("code")
        .to_owned();
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| entry.get("preview").and_then(Value::as_str))
        .unwrap_or("")
        .to_owned();
    let mut spans = vec![
        section_badge_with_palette(&label, palette.accent_secondary, palette),
        Span::raw(" "),
        Span::styled(
            code_location_label(&path, entry),
            Style::default().fg(palette.accent_info),
        ),
        Span::raw(" "),
        Span::styled(
            truncate_inline_text(&name, 120),
            Style::default().fg(palette.text_primary),
        ),
    ];
    if let Some(container) = entry.get("container_name").and_then(Value::as_str) {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            format!("in {container}"),
            Style::default().fg(palette.text_muted),
        ));
    }
    Some(spans)
}

pub(in crate::ui::tool_card) fn code_location_label(path: &str, entry: &Value) -> String {
    let line = range_start_line(entry);
    match range_start_character(entry) {
        Some(character) if character > 0 => format!("{path}:{line}:{character}"),
        _ => format!("{path}:{line}"),
    }
}

pub(in crate::ui::tool_card) fn range_start_line(entry: &Value) -> u64 {
    entry
        .get("range")
        .and_then(|range| range.get("start_line"))
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

pub(in crate::ui::tool_card) fn range_start_character(entry: &Value) -> Option<u64> {
    entry
        .get("range")
        .and_then(|range| range.get("start_character"))
        .and_then(Value::as_u64)
}

pub(in crate::ui::tool_card) fn code_intelligence_source_label(
    server: &str,
    capability: &str,
) -> &'static str {
    if server.starts_with("tree-sitter") || capability.starts_with("tree_sitter/") {
        "Tree-sitter"
    } else if capability.starts_with("textDocument/") || capability.starts_with("workspace/") {
        "LSP"
    } else {
        "Code"
    }
}

pub(in crate::ui::tool_card) fn code_intelligence_capability_label(capability: &str) -> String {
    match capability {
        "textDocument/documentSymbol" | "tree_sitter/document_symbols" => {
            "document symbols".to_owned()
        }
        "workspace/symbol" | "tree_sitter/workspace_symbols" => "workspace symbols".to_owned(),
        "textDocument/definition" => "definition".to_owned(),
        "textDocument/references" => "references".to_owned(),
        "textDocument/diagnostic"
        | "textDocument/publishDiagnostics"
        | "tree_sitter/diagnostics" => "diagnostics".to_owned(),
        other => other.replace('/', " / "),
    }
}

pub(in crate::ui::tool_card) fn code_intelligence_servers_line_with_palette(
    value: &Value,
    palette: &ThemePalette,
) -> Option<Vec<Span<'static>>> {
    let servers = value.get("servers").and_then(Value::as_array)?;
    if servers.len() <= 1 {
        return None;
    }
    let mut labels = servers
        .iter()
        .take(3)
        .filter_map(code_intelligence_server_label)
        .collect::<Vec<_>>();
    let hidden = servers.len().saturating_sub(labels.len());
    if hidden > 0 {
        labels.push(format!("+{hidden} more"));
    }
    if labels.is_empty() {
        return None;
    }
    Some(vec![
        section_badge_with_palette("servers", palette.accent_info, palette),
        Span::raw(" "),
        Span::styled(labels.join(" · "), Style::default().fg(palette.text_muted)),
    ])
}

pub(in crate::ui::tool_card) fn code_intelligence_server_label(value: &Value) -> Option<String> {
    let server = value.get("server").and_then(Value::as_str)?;
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("ready");
    let languages = value
        .get("languages")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(2)
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|languages| !languages.is_empty());
    Some(match languages {
        Some(languages) => format!("{server} {status} ({languages})"),
        None => format!("{server} {status}"),
    })
}

#[cfg(test)]
pub(in crate::ui::tool_card) fn diagnostic_severity_color(severity: &str) -> Color {
    let palette = crate::ui::theme::default_palette();
    diagnostic_severity_color_with_palette(severity, &palette)
}

pub(in crate::ui::tool_card) fn diagnostic_severity_color_with_palette(
    severity: &str,
    palette: &ThemePalette,
) -> Color {
    match severity {
        "error" => palette.status_error,
        "warning" => palette.status_warning,
        _ => palette.accent_secondary,
    }
}
pub(in crate::ui::tool_card) fn code_intelligence_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "code_symbols")
        || tool_name_matches(&summary.tool_name, "code_workspace_symbols")
        || tool_name_matches(&summary.tool_name, "code_definition")
        || tool_name_matches(&summary.tool_name, "code_references")
        || tool_name_matches(&summary.tool_name, "code_diagnostics")
        || tool_name_matches(&summary.tool_name, "code_actions")
}
