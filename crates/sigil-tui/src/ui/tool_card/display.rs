use super::*;

pub(super) fn build_tool_card_display(summary: &ToolCardRender) -> ToolCardDisplay {
    ToolCardDisplay {
        title: tool_action_title(summary),
        status: tool_display_status(summary),
        summary: tool_display_summary(summary),
    }
}

pub(super) fn build_tool_activity_view(summary: &ToolCardRender, source: &str) -> ToolActivityView {
    let display = build_tool_card_display(summary);
    ToolActivityView {
        key: tool_activity_key(summary, source),
        title: display.title.plain(),
        is_inspection: tool_activity_is_inspection_summary(summary),
        defaults_expanded: summary.diff.is_some()
            || terminal_task_is_active(summary)
            || checkpoint_restore_tool(summary),
    }
}

pub(super) fn tool_display_status(summary: &ToolCardRender) -> ToolCardDisplayStatus {
    if checkpoint_restore_tool(summary) {
        return checkpoint_restore_display_status(summary);
    }
    if terminal_task_tool(summary) {
        return terminal_task_display_status(summary);
    }
    if agent_tool(summary)
        && !summary.is_error
        && let Some(status) = agent_payload_string(summary, "status")
    {
        return agent_tool_display_status(&status);
    }
    let label = if summary.is_error {
        match summary.error_kind.as_deref() {
            Some("approval_denied") | Some("permission_denied") => "DENIED",
            Some("interrupted") => "INTERRUPTED",
            Some("timeout") => "TIMEOUT",
            _ => "ERROR",
        }
    } else {
        "OK"
    };
    let detail = if tool_name_matches(&summary.tool_name, "bash") {
        let mut details = Vec::new();
        if let Some(shell) = shell_runtime_label(&summary.metadata) {
            details.push(shell);
        }
        if let Some(backend) = execution_backend_label(&summary.metadata) {
            details.push(backend);
        }
        if let Some(code) = summary.metadata.exit_code {
            details.push(format!("exit {code}"));
        }
        if let Some(verdict) = summary.metadata.shell_verdict.as_deref() {
            details.push(verdict.to_owned());
        }
        if let Some(network_policy) = summary
            .metadata
            .execution_network_policy
            .as_deref()
            .filter(|policy| *policy != "unknown")
        {
            details.push(format!("network {network_policy}"));
        }
        if let Some(timeout_source) = summary
            .metadata
            .execution_timeout_source
            .as_deref()
            .filter(|source| *source != "none")
        {
            details.push(format!("timeout {timeout_source}"));
        }
        if let Some(cleanup_status) = summary
            .metadata
            .execution_cleanup_status
            .as_deref()
            .filter(|status| *status != "not_needed")
        {
            details.push(format!("cleanup {cleanup_status}"));
        }
        if details.is_empty() {
            None
        } else {
            Some(details.join(" · "))
        }
    } else {
        summary
            .metadata
            .mcp_trust_class
            .as_deref()
            .map(|trust_class| format!("trust {trust_class}"))
    };
    ToolCardDisplayStatus {
        label,
        detail,
        kind: if summary.is_error {
            StatusKind::Error
        } else {
            StatusKind::Success
        },
        is_error: summary.is_error,
    }
}

pub(super) fn shell_runtime_label(metadata: &ToolCardMetadata) -> Option<String> {
    metadata.shell_dialect.as_deref().map(|dialect| {
        let dialect = match dialect {
            "powershell" => "PowerShell",
            "posix" => "POSIX shell",
            "cmd" => "cmd.exe",
            other => other,
        };
        metadata
            .shell_program
            .as_deref()
            .filter(|program| !program.is_empty())
            .map(|program| format!("{dialect} ({program})"))
            .unwrap_or_else(|| dialect.to_owned())
    })
}

pub(super) fn execution_backend_label(metadata: &ToolCardMetadata) -> Option<String> {
    metadata.execution_backend.as_deref().map(|backend| {
        if backend == "local" {
            "local unconfined".to_owned()
        } else {
            backend.to_owned()
        }
    })
}

pub(super) fn tool_display_summary(summary: &ToolCardRender) -> Option<String> {
    if agent_tool(summary)
        && let Some(summary) = agent_tool_display_summary(summary)
    {
        return Some(summary);
    }
    if tool_name_matches(&summary.tool_name, "bash")
        && !summary.is_error
        && summary.preview_lines.is_empty()
        && summary.preview_value.is_none()
        && summary.hidden_lines == 0
    {
        return Some("(no output)".to_owned());
    }
    if let Some(diff) = &summary.diff {
        return Some(format!("diff {}", diff.summary));
    }
    summary.summary.clone()
}

pub(super) fn tool_action_title(summary: &ToolCardRender) -> ToolCardTitle {
    if checkpoint_restore_tool(summary) {
        return match summary.metadata.action.as_deref() {
            Some("restored") => ToolCardTitle::new("Restored", "checkpoint files", None),
            _ => ToolCardTitle::new("Review", "checkpoint restore", None),
        };
    }
    if terminal_task_tool(summary) {
        return ToolCardTitle::new(
            "Terminal",
            summary
                .metadata
                .terminal_task_id
                .clone()
                .unwrap_or_else(|| "task".to_owned()),
            summary
                .metadata
                .terminal_command
                .as_deref()
                .map(|command| truncate_inline_text(command, 96)),
        );
    }
    if tool_name_matches(&summary.tool_name, "bash") {
        let command = call_argument(summary, "command")
            .or_else(|| summary.metadata.call_summary.clone())
            .unwrap_or_else(|| summary.tool_name.clone());
        if matches!(
            summary.metadata.shell_command_family.as_deref(),
            Some("check_touched")
        ) {
            let tier = command
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .find_map(|pair| (pair[0] == "--tier").then_some(pair[1]))
                .or_else(|| {
                    command
                        .split_whitespace()
                        .find_map(|part| part.strip_prefix("--tier="))
                });
            return ToolCardTitle::new(
                "Ran",
                tier.map(|tier| format!("check-touched {tier}"))
                    .unwrap_or_else(|| "check-touched".to_owned()),
                None,
            );
        }
        if !summary.is_error
            && let Some(search) = classify_simple_shell_search(&command)
        {
            return ToolCardTitle::new(
                "Searched",
                search.pattern,
                search.location.map(|location| format!("in {location}")),
            );
        }
        return shell_command_title("Ran", &command);
    }
    if tool_name_matches(&summary.tool_name, "read_file") {
        return ToolCardTitle::new("Read", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "write_file") {
        return ToolCardTitle::new(write_file_action(summary), primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "edit_file") {
        return ToolCardTitle::new("Edited", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "delete_file") {
        return ToolCardTitle::new("Deleted", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "code_action") {
        return ToolCardTitle::new(
            "Applied",
            primary_path(summary),
            Some("code action".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_rename") {
        return ToolCardTitle::new("Renamed", primary_path(summary), Some("symbol".to_owned()));
    }
    if tool_name_matches(&summary.tool_name, "grep") {
        let pattern = call_argument(summary, "pattern").unwrap_or_else(|| "pattern".to_owned());
        let path = call_argument(summary, "path").unwrap_or_else(|| "workspace".to_owned());
        return ToolCardTitle::new("Searched", pattern, Some(format!("in {path}")));
    }
    if tool_name_matches(&summary.tool_name, "glob") {
        return ToolCardTitle::new(
            "Searched",
            call_argument(summary, "pattern").unwrap_or_else(|| summary.tool_name.clone()),
            None,
        );
    }
    if tool_name_matches(&summary.tool_name, "ls") {
        return ToolCardTitle::new("Listed", primary_path(summary), None);
    }
    if tool_name_matches(&summary.tool_name, "code_symbols") {
        return ToolCardTitle::new(
            "Inspected",
            primary_path(summary),
            Some("symbols".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_workspace_symbols") {
        return ToolCardTitle::new(
            "Searched",
            call_argument(summary, "query").unwrap_or_else(|| "symbols".to_owned()),
            Some("workspace".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_definition") {
        return ToolCardTitle::new(
            "Located",
            primary_path(summary),
            Some("definition".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_references") {
        return ToolCardTitle::new(
            "Searched",
            primary_path(summary),
            Some("references".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_actions") {
        return ToolCardTitle::new(
            "Inspected",
            primary_path(summary),
            Some("actions".to_owned()),
        );
    }
    if tool_name_matches(&summary.tool_name, "code_diagnostics") {
        return ToolCardTitle::new(
            "Checked",
            primary_path(summary),
            Some("diagnostics".to_owned()),
        );
    }
    if agent_tool(summary) {
        return agent_tool_title(summary);
    }
    if let Some(mcp) = mcp_tool_display(summary) {
        return ToolCardTitle::new("Called", mcp.tool, Some(format!("on {}", mcp.server)));
    }
    match &summary.metadata.call_summary {
        Some(call_summary) => ToolCardTitle::new(
            "Called",
            summary.tool_name.clone(),
            Some(sanitize_call_summary(call_summary)),
        ),
        None => ToolCardTitle::new("Called", summary.tool_name.clone(), None),
    }
}

pub(super) fn tool_title_spans_with_palette(
    title: &ToolCardTitle,
    max_chars: usize,
    palette: &ThemePalette,
) -> Vec<Span<'static>> {
    let action_style = Style::default()
        .fg(palette.accent_warning)
        .add_modifier(Modifier::BOLD);
    let subject_style = Style::default()
        .fg(palette.accent_info)
        .add_modifier(Modifier::BOLD);
    let args_style = Style::default().fg(palette.text_primary);
    let segments = title_segments(title, action_style, subject_style, args_style);
    let plain_len = title.plain().chars().count();
    if plain_len <= max_chars {
        return segments
            .into_iter()
            .map(|(segment, style)| Span::styled(segment, style))
            .collect();
    }

    let mut remaining = max_chars.saturating_sub(3).max(1);
    let mut spans = Vec::new();
    for (segment, style) in segments {
        let segment_len = segment.chars().count();
        if segment_len <= remaining {
            spans.push(Span::styled(segment, style));
            remaining -= segment_len;
            if remaining == 0 {
                spans.push(Span::styled("...", style));
                break;
            }
            continue;
        }
        let truncated = segment.chars().take(remaining).collect::<String>();
        spans.push(Span::styled(format!("{truncated}..."), style));
        break;
    }
    if spans.is_empty() {
        spans.push(Span::styled("...", args_style));
    }
    spans
}

pub(super) fn title_segments(
    title: &ToolCardTitle,
    action_style: Style,
    subject_style: Style,
    args_style: Style,
) -> Vec<(String, Style)> {
    let mut segments = vec![
        (title.action.clone(), action_style),
        (" ".to_owned(), Style::default()),
        (title.subject.clone(), subject_style),
    ];
    if let Some(args) = &title.args
        && !args.is_empty()
    {
        segments.push((" ".to_owned(), Style::default()));
        segments.push((args.clone(), args_style));
    }
    segments
}

pub(super) fn shell_command_title(action: &'static str, command: &str) -> ToolCardTitle {
    let command = command.trim();
    let mut parts = command.splitn(2, char::is_whitespace);
    let subject = parts
        .next()
        .filter(|part| !part.is_empty())
        .unwrap_or(command);
    let args = parts
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    ToolCardTitle::new(action, subject, args)
}

pub(super) fn tool_activity_is_inspection_summary(summary: &ToolCardRender) -> bool {
    if checkpoint_restore_tool(summary) {
        return true;
    }
    if tool_name_matches(&summary.tool_name, "read_file")
        || tool_name_matches(&summary.tool_name, "grep")
        || tool_name_matches(&summary.tool_name, "glob")
        || tool_name_matches(&summary.tool_name, "ls")
    {
        return true;
    }
    tool_name_matches(&summary.tool_name, "bash")
        && !summary.is_error
        && call_argument(summary, "command")
            .or_else(|| summary.metadata.call_summary.clone())
            .and_then(|command| classify_simple_shell_search(&command))
            .is_some()
}

fn checkpoint_restore_tool(summary: &ToolCardRender) -> bool {
    tool_name_matches(&summary.tool_name, "checkpoint_restore")
}

fn checkpoint_restore_display_status(summary: &ToolCardRender) -> ToolCardDisplayStatus {
    match summary.metadata.action.as_deref() {
        Some("restored") => ToolCardDisplayStatus {
            label: "RESTORED",
            detail: Some("verification stale".to_owned()),
            kind: StatusKind::Success,
            is_error: false,
        },
        Some("blocked") => ToolCardDisplayStatus {
            label: "BLOCKED",
            detail: Some("preflight conflict".to_owned()),
            kind: StatusKind::Error,
            is_error: true,
        },
        _ => ToolCardDisplayStatus {
            label: "PREVIEW",
            detail: Some("Enter to restore".to_owned()),
            kind: StatusKind::Pending,
            is_error: false,
        },
    }
}

pub(super) fn tool_activity_key(summary: &ToolCardRender, source: &str) -> String {
    if terminal_task_tool(summary)
        && let Some(task_id) = &summary.metadata.terminal_task_id
    {
        return format!("terminal_task:{task_id}");
    }
    summary
        .call_id
        .as_ref()
        .map(|call_id| format!("call:{call_id}"))
        .unwrap_or_else(|| format!("hash:{:016x}", stable_tool_activity_hash(source)))
}

pub(super) fn stable_tool_activity_hash(source: &str) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(super) fn write_file_action(summary: &ToolCardRender) -> &'static str {
    if summary
        .diff
        .as_ref()
        .is_some_and(|diff| diff.files.iter().all(diff_file_is_create))
    {
        "Created"
    } else {
        "Wrote"
    }
}

pub(super) fn diff_file_is_create(file: &ToolCardDiffFile) -> bool {
    let (added, removed) = file_diff_line_stats(file);
    added > 0 && removed == 0
}

pub(super) fn primary_path(summary: &ToolCardRender) -> String {
    call_argument(summary, "path")
        .or_else(|| summary.metadata.changed_files.first().cloned())
        .or_else(|| {
            summary
                .diff
                .as_ref()?
                .files
                .first()
                .map(|file| file.path.clone())
        })
        .unwrap_or_else(|| "workspace".to_owned())
}

pub(super) fn call_argument(summary: &ToolCardRender, key: &str) -> Option<String> {
    let call_summary = summary.metadata.call_summary.as_deref()?;
    call_summary_argument(call_summary, key)
}

pub(super) fn call_summary_argument(call_summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let start = call_summary.find(&prefix)? + prefix.len();
    if key == "command" {
        return Some(call_summary[start..].trim().to_owned());
    }
    let tail = &call_summary[start..];
    let end = tail
        .find(|character: char| character.is_whitespace())
        .unwrap_or(tail.len());
    Some(tail[..end].trim().to_owned()).filter(|value| !value.is_empty())
}

pub(super) struct McpToolDisplay {
    pub(super) server: String,
    pub(super) tool: String,
}

pub(super) fn mcp_tool_display(summary: &ToolCardRender) -> Option<McpToolDisplay> {
    let (server_from_name, tool_from_name) = parse_mcp_provider_name(&summary.tool_name)
        .map(|(server, tool)| (Some(server), Some(tool)))
        .unwrap_or((None, None));
    let server = summary.metadata.mcp_server.clone().or(server_from_name)?;
    let tool = summary
        .metadata
        .mcp_tool
        .clone()
        .or(tool_from_name)
        .unwrap_or_else(|| summary.tool_name.clone());
    Some(McpToolDisplay { server, tool })
}

pub(super) fn parse_mcp_provider_name(tool_name: &str) -> Option<(String, String)> {
    let remainder = tool_name.strip_prefix("mcp__")?;
    let (server, tool) = remainder.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

pub(super) fn sanitize_call_summary(call_summary: &str) -> String {
    truncate_inline_text(
        &call_summary
            .split_whitespace()
            .filter(|part| !part.starts_with("call_") && !part.starts_with("id="))
            .collect::<Vec<_>>()
            .join(" "),
        120,
    )
}
