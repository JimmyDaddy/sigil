use std::{
    borrow::Cow,
    time::{SystemTime, UNIX_EPOCH},
};

use ratatui::text::Line;
use sigil_kernel::{
    AgentInvocationMode, AgentInvocationSource, AgentThreadStartedEntry, AgentThreadStatus,
    AgentThreadStatusChangedEntry, ReasoningEffort, RootConfig, SecretRedactor, TerminalTaskEntry,
    TerminalTaskStatus, ToolCall, ToolExecutionEntry, ToolExecutionStatus, ToolPreviewSnapshot,
    ToolResult, ToolResultMeta, ToolResultStatus,
};

use crate::slash::KNOWN_MODEL_IDS;

use super::file_type::{
    path_has_code_or_data_extension, path_has_document_extension, path_language,
};

const TOOL_DISPLAY_CONTENT_MAX_BYTES: usize = 64 * 1024;
const RESTORED_TOOL_ENVELOPE_PARSE_MAX_BYTES: usize = 128 * 1024;

pub(super) fn human_file_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KB", bytes as f64 / KB);
    }
    format!("{:.1} MB", bytes as f64 / MB)
}

pub(super) fn relative_age_label(modified_epoch_secs: u64) -> String {
    let now_epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(modified_epoch_secs);
    let delta = now_epoch_secs.saturating_sub(modified_epoch_secs);
    match delta {
        0..=59 => format!("{delta}s ago"),
        60..=3599 => format!("{}m ago", delta / 60),
        3600..=86399 => format!("{}h ago", delta / 3600),
        _ => format!("{}d ago", delta / 86_400),
    }
}

pub(super) fn summarize_error(error: &str) -> String {
    error
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "Caused by:")
        .map(strip_error_chain_prefix)
        .next_back()
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| error.trim())
        .to_owned()
}

fn strip_error_chain_prefix(line: &str) -> &str {
    if let Some((prefix, rest)) = line.split_once(':')
        && prefix.trim().chars().all(|char| char.is_ascii_digit())
    {
        return rest.trim();
    }
    line.trim()
}

pub(super) fn truncate_session_view_text(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let truncated = normalized.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

pub(super) fn parse_reasoning_effort(value: &str) -> Option<ReasoningEffort> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Some(ReasoningEffort::Low),
        "medium" | "med" => Some(ReasoningEffort::Medium),
        "high" => Some(ReasoningEffort::High),
        "max" => Some(ReasoningEffort::Max),
        _ => None,
    }
}

pub(super) fn sidebar_width_for_terminal(total_width: usize) -> usize {
    if total_width < 96 {
        return 0;
    }
    if total_width < 132 {
        return 24;
    }
    ((total_width * 28) / 100).clamp(28, 42)
}

pub(super) fn normalize_command_prefix_character(character: char) -> Option<char> {
    match character {
        '/' | '、' => Some('/'),
        _ => None,
    }
}

pub(super) fn format_token_count(tokens: u64) -> String {
    let digits = tokens.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(character);
    }
    out
}

pub(super) fn format_agent_thread_started_block(entry: &AgentThreadStartedEntry) -> String {
    let mode = agent_invocation_mode_label(entry.invocation_mode);
    let mut preview_value = serde_json::json!({
        "thread_id": entry.thread_id.as_str(),
        "profile_id": entry.profile_id.as_str(),
        "display_name": entry
            .display_name
            .as_deref()
            .unwrap_or_else(|| entry.profile_id.as_str()),
        "status": "running",
        "mode": mode,
        "source": agent_invocation_source_label(entry.invocation_source),
        "reason": "waiting for result",
    });
    if entry.invocation_mode == AgentInvocationMode::JoinBeforeFinal
        && let Some(object) = preview_value.as_object_mut()
    {
        object.insert(
            "action_hint".to_owned(),
            serde_json::Value::String("Ctrl-B background".to_owned()),
        );
    }
    serde_json::json!({
        "call_id": format!("agent-started-{}", entry.thread_id.as_str()),
        "tool_name": "spawn_agent",
        "status": "ok",
        "summary": if entry.invocation_mode == AgentInvocationMode::JoinBeforeFinal {
            "join before final · Ctrl-B background"
        } else {
            mode
        },
        "preview_kind": "json",
        "preview_value": preview_value,
        "preview_lines": [],
        "hidden_lines": 0,
    })
    .to_string()
}

pub(super) fn format_agent_thread_status_block(entry: &AgentThreadStatusChangedEntry) -> String {
    let status = agent_thread_status_label(entry.status);
    let mut preview_value = serde_json::json!({
        "thread_id": entry.thread_id.as_str(),
        "status": status,
    });
    if let Some(reason) = entry.reason.as_deref()
        && let Some(object) = preview_value.as_object_mut()
    {
        object.insert(
            "reason".to_owned(),
            serde_json::Value::String(reason.to_owned()),
        );
    }
    serde_json::json!({
        "call_id": format!("agent-status-{}-{}", entry.thread_id.as_str(), status),
        "tool_name": "wait_agent",
        "status": "ok",
        "summary": entry
            .reason
            .as_deref()
            .unwrap_or(status),
        "preview_kind": "json",
        "preview_value": preview_value,
        "preview_lines": [],
        "hidden_lines": 0,
    })
    .to_string()
}

fn agent_invocation_mode_label(mode: AgentInvocationMode) -> &'static str {
    match mode {
        AgentInvocationMode::Foreground => "foreground",
        AgentInvocationMode::Background => "background",
        AgentInvocationMode::JoinBeforeFinal => "join_before_final",
        AgentInvocationMode::Unknown => "unknown",
    }
}

fn agent_invocation_source_label(source: AgentInvocationSource) -> &'static str {
    match source {
        AgentInvocationSource::Chat => "chat",
        AgentInvocationSource::Mention => "mention",
        AgentInvocationSource::Skill => "skill",
        AgentInvocationSource::Task => "task",
        AgentInvocationSource::Plugin => "plugin",
        AgentInvocationSource::System => "system",
        AgentInvocationSource::Unknown => "unknown",
    }
}

fn agent_thread_status_label(status: AgentThreadStatus) -> &'static str {
    match status {
        AgentThreadStatus::Started => "started",
        AgentThreadStatus::Running => "running",
        AgentThreadStatus::Blocked => "blocked",
        AgentThreadStatus::Completed => "completed",
        AgentThreadStatus::Failed => "failed",
        AgentThreadStatus::Cancelled => "cancelled",
        AgentThreadStatus::Interrupted => "interrupted",
        AgentThreadStatus::Closed => "closed",
        AgentThreadStatus::Unavailable => "unavailable",
        AgentThreadStatus::Unknown => "unknown",
    }
}

pub(super) fn format_token_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        return format!("{:.1}M", tokens as f64 / 1_000_000.0);
    }
    if tokens >= 1_000 {
        return format!("{:.1}K", tokens as f64 / 1_000.0);
    }
    tokens.to_string()
}

pub(super) fn line_has_visible_content(line: &Line<'_>) -> bool {
    line.spans.iter().any(|span| {
        !span
            .content
            .as_ref()
            .trim_matches(|character: char| character.is_whitespace() || character == '▌')
            .is_empty()
    })
}

pub(super) fn plain_line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

pub(super) fn hash_timeline_line(seed: u64, line: &str) -> u64 {
    let mut hash = seed;
    for byte in line.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(1_099_511_628_211);
    }
    hash ^= 0xff;
    hash.wrapping_mul(1_099_511_628_211)
}

pub(super) fn ratio_to_percent(ratio: f32) -> u32 {
    (ratio * 100.0).round().clamp(0.0, 999.0) as u32
}

pub(super) fn format_tool_result_block_redacted(
    result: &ToolResult,
    preview: Option<&ToolPreviewSnapshot>,
    redactor: &SecretRedactor,
) -> String {
    let preview = if result.is_error() { None } else { preview };
    let error_kind = tool_result_error_kind(result);
    format_tool_preview_payload(
        Some(result.call_id.as_str()),
        result.tool_name.as_str(),
        if result.is_error() { "error" } else { "ok" },
        &result.content,
        Some(&result.metadata),
        preview,
        error_kind,
        redactor,
    )
}

pub(super) fn format_terminal_task_block_redacted(
    entry: &TerminalTaskEntry,
    redactor: &SecretRedactor,
) -> String {
    let output_preview = entry
        .output_preview
        .as_deref()
        .map(|preview| redactor.redact_text(preview));
    let preview_lines = output_preview
        .as_deref()
        .map(|preview| preview.lines().map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    let hidden_lines = output_preview
        .as_deref()
        .map(|_| 0usize)
        .unwrap_or(0)
        .saturating_add(usize::from(entry.output_truncated));
    let details = serde_json::json!({
        "terminal_task": {
            "task_id": entry.handle.task_id.as_str(),
            "status": entry.status.as_str(),
            "status_detail": &entry.status,
            "command": &entry.handle.command,
            "cwd": &entry.handle.cwd,
            "shell": &entry.handle.shell,
            "log_path": &entry.handle.log_path,
            "created_at_ms": entry.handle.created_at_ms,
            "updated_at_ms": entry.updated_at_ms,
            "output_hash": &entry.output_hash,
            "output_truncated": entry.output_truncated,
            "enforcement_backend": &entry.handle.enforcement_backend,
            "enforcement_backend_capabilities": &entry.handle.enforcement_backend_capabilities,
            "sandbox_profile": &entry.handle.sandbox_profile,
            "cleanup": &entry.cleanup
        }
    });
    let status = if matches!(entry.status, TerminalTaskStatus::Failed { .. }) {
        "error"
    } else {
        "ok"
    };
    let summary = format!(
        "{} · {}",
        terminal_task_summary_status(&entry.status),
        redactor.redact_text(&entry.handle.command)
    );
    let object = serde_json::json!({
        "tool_name": "terminal_task",
        "status": status,
        "summary": summary,
        "preview_kind": "text",
        "preview_lines": preview_lines,
        "hidden_lines": hidden_lines,
        "metadata": {
            "truncated": entry.output_truncated,
            "details": redactor.redact_value(&details)
        }
    });
    serde_json::to_string(&object).unwrap_or_else(|_| {
        r#"{"tool_name":"terminal_task","status":"error","summary":"failed to serialize terminal task payload","preview_kind":"text","preview_lines":[],"hidden_lines":0}"#
            .to_owned()
    })
}

pub(super) fn format_tool_content_block_redacted_for_restore(
    call_id: Option<&str>,
    content: &str,
    execution: Option<&ToolExecutionEntry>,
    tool_call: Option<&ToolCall>,
    preview: Option<&ToolPreviewSnapshot>,
    redactor: &SecretRedactor,
) -> String {
    let envelope = parse_restored_tool_result_envelope(content);
    let display_content = envelope
        .as_ref()
        .and_then(|value| value.get("content"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or(content);
    let status = envelope
        .as_ref()
        .and_then(|value| value.get("status"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| execution.and_then(restored_execution_status_label))
        .unwrap_or("ok");
    let preview = if status == "ok" { preview } else { None };
    let tool_name = execution
        .map(|entry| entry.tool_name.as_str())
        .or_else(|| tool_call.map(|call| call.name.as_str()))
        .unwrap_or("tool");
    let metadata = restored_tool_metadata(envelope.as_ref(), execution, tool_call);
    let error_kind = restored_tool_error_kind(envelope.as_ref(), execution);
    format_tool_preview_payload(
        call_id,
        tool_name,
        status,
        display_content,
        metadata.as_ref(),
        preview,
        error_kind.as_deref(),
        redactor,
    )
}

fn terminal_task_summary_status(status: &TerminalTaskStatus) -> String {
    match status {
        TerminalTaskStatus::Exited {
            exit_code: Some(code),
        } => format!("exited {code}"),
        TerminalTaskStatus::Failed { reason } => {
            format!("failed {}", truncate_session_view_text(reason, 48))
        }
        other => other.as_str().to_owned(),
    }
}

fn format_tool_preview_payload(
    call_id: Option<&str>,
    tool_name: &str,
    status: &str,
    content: &str,
    metadata: Option<&ToolResultMeta>,
    preview: Option<&ToolPreviewSnapshot>,
    error_kind: Option<&str>,
    redactor: &SecretRedactor,
) -> String {
    let original_bytes = content.len() as u64;
    let (display_content, display_truncated) = bounded_tool_display_content(content);
    let content = redactor.redact_text(display_content.as_ref());
    let preview_value = tool_preview_value(&content)
        .or_else(|| agent_tool_metadata_preview_value(tool_name, metadata));
    let (preview_kind, preview_source) =
        tool_preview_source(tool_name, &content, preview_value.as_ref(), metadata);
    let preview_language = tool_preview_language(tool_name, preview_kind, metadata);
    let all_lines = if preview_source.is_empty() {
        Vec::new()
    } else {
        preview_source
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let total_lines = all_lines.len();
    let preview_lines = all_lines;
    let hidden_lines = 0usize;
    let bytes = metadata
        .and_then(|value| value.bytes)
        .unwrap_or(original_bytes);
    let mut object = serde_json::Map::new();
    if let Some(call_id) = call_id {
        object.insert(
            "call_id".to_owned(),
            serde_json::Value::String(call_id.to_owned()),
        );
    }
    object.insert(
        "tool_name".to_owned(),
        serde_json::Value::String(tool_name.to_owned()),
    );
    object.insert(
        "status".to_owned(),
        serde_json::Value::String(status.to_owned()),
    );
    if let Some(error_kind) = error_kind {
        object.insert(
            "error_kind".to_owned(),
            serde_json::Value::String(error_kind.to_owned()),
        );
    }
    object.insert(
        "preview_kind".to_owned(),
        serde_json::Value::String(preview_kind.to_owned()),
    );
    if let Some(preview_language) = preview_language {
        object.insert(
            "preview_language".to_owned(),
            serde_json::Value::String(preview_language),
        );
    }
    let diff_payload = preview.and_then(|preview| format_tool_diff_payload(preview, redactor));
    let mut summary = format_tool_preview_summary(
        tool_name,
        total_lines,
        preview_lines.len(),
        hidden_lines,
        bytes,
    );
    if let Some((diff_summary, _)) = diff_payload.as_ref() {
        summary.push_str(" · diff ");
        summary.push_str(diff_summary);
    }
    if display_truncated {
        summary.push_str(" · display truncated");
    }
    object.insert("summary".to_owned(), serde_json::Value::String(summary));
    object.insert(
        "preview_lines".to_owned(),
        serde_json::Value::Array(
            preview_lines
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    object.insert(
        "hidden_lines".to_owned(),
        serde_json::Value::Number(hidden_lines.into()),
    );
    if display_truncated {
        object.insert(
            "display_truncated".to_owned(),
            serde_json::Value::Bool(true),
        );
    }
    if let Some(metadata) = metadata {
        object.insert(
            "metadata".to_owned(),
            redactor
                .redact_value(&serde_json::to_value(metadata).unwrap_or(serde_json::Value::Null)),
        );
    }
    if let Some(preview_value) = preview_value {
        object.insert("preview_value".to_owned(), preview_value);
    }
    if let Some((_, diff)) = diff_payload {
        object.insert("diff".to_owned(), diff);
    }
    serde_json::to_string(&serde_json::Value::Object(object)).unwrap_or(content)
}

fn tool_result_error_kind(result: &ToolResult) -> Option<&str> {
    match &result.status {
        ToolResultStatus::Ok => None,
        ToolResultStatus::Error(error) => Some(error.kind.as_str()),
    }
}

fn format_tool_diff_payload(
    preview: &ToolPreviewSnapshot,
    redactor: &SecretRedactor,
) -> Option<(String, serde_json::Value)> {
    if preview.file_diffs.is_empty() {
        return None;
    }
    let file_count = preview.changed_files.len().max(preview.file_diffs.len());
    let file_label = if file_count == 1 { "file" } else { "files" };
    let mut summary = format!(
        "+{} -{} · {} {}",
        preview.original_stats.added, preview.original_stats.removed, file_count, file_label
    );
    if preview.truncated {
        summary.push_str(" · truncated");
    }

    let files = preview
        .file_diffs
        .iter()
        .map(|file| {
            let mut object = serde_json::Map::new();
            object.insert(
                "path".to_owned(),
                serde_json::Value::String(file.path.clone()),
            );
            object.insert(
                "lines".to_owned(),
                serde_json::Value::Array(
                    file.diff
                        .lines()
                        .map(|line| serde_json::Value::String(redactor.redact_text(line)))
                        .collect(),
                ),
            );
            object.insert(
                "truncated".to_owned(),
                serde_json::Value::Bool(file.truncated),
            );
            object.insert(
                "original_line_count".to_owned(),
                serde_json::Value::Number(file.original_line_count.into()),
            );
            object.insert(
                "rendered_line_count".to_owned(),
                serde_json::Value::Number(file.rendered_line_count.into()),
            );
            object.insert(
                "original_stats".to_owned(),
                serde_json::to_value(file.original_stats).unwrap_or(serde_json::Value::Null),
            );
            object.insert(
                "rendered_stats".to_owned(),
                serde_json::to_value(file.rendered_stats).unwrap_or(serde_json::Value::Null),
            );
            serde_json::Value::Object(object)
        })
        .collect::<Vec<_>>();

    let mut object = serde_json::Map::new();
    object.insert(
        "summary".to_owned(),
        serde_json::Value::String(summary.clone()),
    );
    object.insert(
        "truncated".to_owned(),
        serde_json::Value::Bool(preview.truncated),
    );
    object.insert(
        "original_line_count".to_owned(),
        serde_json::Value::Number(preview.original_line_count.into()),
    );
    object.insert(
        "rendered_line_count".to_owned(),
        serde_json::Value::Number(preview.rendered_line_count.into()),
    );
    object.insert(
        "original_stats".to_owned(),
        serde_json::to_value(preview.original_stats).unwrap_or(serde_json::Value::Null),
    );
    object.insert(
        "rendered_stats".to_owned(),
        serde_json::to_value(preview.rendered_stats).unwrap_or(serde_json::Value::Null),
    );
    object.insert("files".to_owned(), serde_json::Value::Array(files));
    Some((summary, serde_json::Value::Object(object)))
}

fn parse_tool_content_value(content: &str) -> serde_json::Value {
    serde_json::from_str(content).unwrap_or_else(|_| serde_json::Value::String(content.to_owned()))
}

fn parse_tool_result_envelope(content: &str) -> Option<serde_json::Value> {
    let value = serde_json::from_str::<serde_json::Value>(content).ok()?;
    let object = value.as_object()?;
    (object.contains_key("status") && object.contains_key("content")).then_some(value)
}

fn parse_restored_tool_result_envelope(content: &str) -> Option<serde_json::Value> {
    if content.len() > RESTORED_TOOL_ENVELOPE_PARSE_MAX_BYTES {
        return None;
    }
    parse_tool_result_envelope(content)
}

fn bounded_tool_display_content(content: &str) -> (Cow<'_, str>, bool) {
    if content.len() <= TOOL_DISPLAY_CONTENT_MAX_BYTES {
        return (Cow::Borrowed(content), false);
    }
    let cutoff = previous_char_boundary(content, TOOL_DISPLAY_CONTENT_MAX_BYTES);
    let mut truncated = String::with_capacity(cutoff + 80);
    truncated.push_str(&content[..cutoff]);
    truncated.push_str("\n[display truncated; original bytes=");
    truncated.push_str(&content.len().to_string());
    truncated.push(']');
    (Cow::Owned(truncated), true)
}

fn previous_char_boundary(value: &str, max_index: usize) -> usize {
    let mut index = max_index.min(value.len());
    while index > 0 && !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn restored_execution_status_label(execution: &ToolExecutionEntry) -> Option<&'static str> {
    match execution.status {
        ToolExecutionStatus::Completed => Some("ok"),
        ToolExecutionStatus::Failed
        | ToolExecutionStatus::Cancelled
        | ToolExecutionStatus::Interrupted => Some("error"),
        ToolExecutionStatus::Started => None,
    }
}

fn restored_tool_metadata(
    envelope: Option<&serde_json::Value>,
    execution: Option<&ToolExecutionEntry>,
    tool_call: Option<&ToolCall>,
) -> Option<ToolResultMeta> {
    let mut metadata = if let Some(execution) = execution {
        Some(execution.metadata.clone())
    } else {
        envelope
            .and_then(|value| value.get("meta"))
            .and_then(project_model_meta_to_tool_result_meta)
    };
    if let Some(tool_call) = tool_call {
        enrich_restored_metadata_from_tool_call(&mut metadata, tool_call);
    }
    metadata
}

fn enrich_restored_metadata_from_tool_call(
    metadata: &mut Option<ToolResultMeta>,
    tool_call: &ToolCall,
) {
    if tool_call.name != "read_file" {
        return;
    }
    let Ok(args) = serde_json::from_str::<serde_json::Value>(&tool_call.args_json) else {
        return;
    };
    let Some(args_object) = args.as_object() else {
        return;
    };
    let metadata = metadata.get_or_insert_with(ToolResultMeta::default);
    let mut details = metadata.details.as_object().cloned().unwrap_or_default();
    let call_details = details
        .entry("call".to_owned())
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(call_object) = call_details.as_object_mut() {
        for key in ["path", "offset", "limit"] {
            if let Some(value) = args_object.get(key) {
                call_object
                    .entry(key.to_owned())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    metadata.details = serde_json::Value::Object(details);
}

fn restored_tool_error_kind(
    envelope: Option<&serde_json::Value>,
    execution: Option<&ToolExecutionEntry>,
) -> Option<String> {
    execution
        .and_then(|entry| entry.error.as_ref())
        .map(|error| error.kind.as_str().to_owned())
        .or_else(|| {
            envelope
                .and_then(|value| value.get("error"))
                .and_then(|error| error.get("kind"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
}

fn project_model_meta_to_tool_result_meta(value: &serde_json::Value) -> Option<ToolResultMeta> {
    let object = value.as_object()?;
    let mut metadata = ToolResultMeta {
        duration_ms: object
            .get("duration_ms")
            .and_then(serde_json::Value::as_u64),
        exit_code: object
            .get("exit_code")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        stdout_bytes: object
            .get("stdout_bytes")
            .and_then(serde_json::Value::as_u64),
        stderr_bytes: object
            .get("stderr_bytes")
            .and_then(serde_json::Value::as_u64),
        bytes: object.get("bytes").and_then(serde_json::Value::as_u64),
        truncated: object
            .get("truncated")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        omitted_bytes: object
            .get("omitted_bytes")
            .and_then(serde_json::Value::as_u64),
        limit_bytes: object
            .get("limit_bytes")
            .and_then(serde_json::Value::as_u64),
        limit_lines: object
            .get("limit_lines")
            .and_then(serde_json::Value::as_u64),
        returned_bytes: object
            .get("returned_bytes")
            .and_then(serde_json::Value::as_u64),
        returned_lines: object
            .get("returned_lines")
            .and_then(serde_json::Value::as_u64),
        total_bytes: object
            .get("total_bytes")
            .and_then(serde_json::Value::as_u64),
        total_lines: object
            .get("total_lines")
            .and_then(serde_json::Value::as_u64),
        returned_matches: object
            .get("returned_matches")
            .and_then(serde_json::Value::as_u64),
        total_matches: object
            .get("total_matches")
            .and_then(serde_json::Value::as_u64),
        returned_entries: object
            .get("returned_entries")
            .and_then(serde_json::Value::as_u64),
        total_entries: object
            .get("total_entries")
            .and_then(serde_json::Value::as_u64),
        changed_files: Vec::new(),
        details: object
            .get("details")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        ..ToolResultMeta::default()
    };
    if let Some(files) = object
        .get("changed_files")
        .and_then(serde_json::Value::as_array)
    {
        metadata.changed_files = files
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect();
    }
    Some(metadata)
}

fn tool_preview_value(content: &str) -> Option<serde_json::Value> {
    let value = parse_tool_content_value(content);
    matches!(
        value,
        serde_json::Value::Array(_) | serde_json::Value::Object(_)
    )
    .then_some(value)
}

fn tool_preview_source(
    tool_name: &str,
    content: &str,
    preview_value: Option<&serde_json::Value>,
    metadata: Option<&ToolResultMeta>,
) -> (&'static str, String) {
    if let Some(source) = agent_tool_preview_source(tool_name, preview_value) {
        return source;
    }
    if tool_name == "read_file" {
        return read_file_tool_preview_source(content, preview_value, metadata);
    }
    if let Some(value) = preview_value {
        let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| content.to_owned());
        return ("json", pretty);
    }
    if looks_like_markdown_document(content) {
        return ("markdown", content.to_owned());
    }
    ("text", content.to_owned())
}

fn read_file_tool_preview_source(
    content: &str,
    preview_value: Option<&serde_json::Value>,
    metadata: Option<&ToolResultMeta>,
) -> (&'static str, String) {
    if let Some(path) = metadata.and_then(read_file_metadata_path) {
        if path_has_document_extension(&path) {
            return ("markdown", content.to_owned());
        }
        if path_has_code_or_data_extension(&path) {
            if let Some(value) = preview_value {
                let pretty =
                    serde_json::to_string_pretty(value).unwrap_or_else(|_| content.to_owned());
                return ("json", pretty);
            }
            return ("code", content.to_owned());
        }
    }
    if let Some(value) = preview_value {
        let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| content.to_owned());
        return ("json", pretty);
    }
    if looks_like_markdown_document(content) {
        return ("markdown", content.to_owned());
    }
    ("text", content.to_owned())
}

fn read_file_metadata_path(metadata: &ToolResultMeta) -> Option<String> {
    metadata
        .details
        .get("call")
        .and_then(|call| call.get("path"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            metadata
                .details
                .get("path")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| {
            metadata
                .details
                .get("call")
                .and_then(|call| call.get("summary"))
                .and_then(serde_json::Value::as_str)
                .and_then(|summary| call_summary_value(summary, "path"))
        })
}

fn tool_preview_language(
    tool_name: &str,
    preview_kind: &str,
    metadata: Option<&ToolResultMeta>,
) -> Option<String> {
    if tool_name != "read_file" || preview_kind != "code" {
        return None;
    }
    metadata.and_then(read_file_metadata_language).or_else(|| {
        metadata
            .and_then(read_file_metadata_path)
            .and_then(path_language)
    })
}

fn read_file_metadata_language(metadata: &ToolResultMeta) -> Option<String> {
    metadata
        .details
        .get("language")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            metadata
                .details
                .get("call")
                .and_then(|call| call.get("language"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .filter(|language| !language.trim().is_empty())
}

fn call_summary_value(call_summary: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    let start = call_summary.find(&prefix)? + prefix.len();
    let tail = &call_summary[start..];
    let end = tail
        .find(|character: char| character.is_whitespace())
        .unwrap_or(tail.len());
    Some(tail[..end].trim().to_owned()).filter(|value| !value.is_empty())
}

fn agent_tool_preview_source(
    tool_name: &str,
    preview_value: Option<&serde_json::Value>,
) -> Option<(&'static str, String)> {
    let value = preview_value?;
    match tool_name {
        "read_agent_result" => Some(("agent_result", String::new())),
        "spawn_agent" => value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .filter(|summary| !summary.trim().is_empty())
            .map(|summary| ("markdown", summary.to_owned()))
            .or_else(|| Some(("text", String::new()))),
        "wait_agent" | "list_agents" | "cancel_agent" | "message_agent" | "close_agent" => {
            Some(("text", String::new()))
        }
        _ => None,
    }
}

fn agent_tool_metadata_preview_value(
    tool_name: &str,
    metadata: Option<&ToolResultMeta>,
) -> Option<serde_json::Value> {
    if !matches!(
        tool_name,
        "spawn_agent"
            | "wait_agent"
            | "read_agent_result"
            | "list_agents"
            | "cancel_agent"
            | "message_agent"
            | "close_agent"
    ) {
        return None;
    }
    let details = &metadata?.details;
    let thread_id = details
        .get("thread_id")
        .and_then(serde_json::Value::as_str)?
        .trim();
    if thread_id.is_empty() {
        return None;
    }
    let mut object = serde_json::Map::new();
    object.insert(
        "thread_id".to_owned(),
        serde_json::Value::String(thread_id.to_owned()),
    );
    for key in [
        "display_name",
        "profile_id",
        "objective",
        "status",
        "reason",
        "coalescing_key",
        "next_action",
    ] {
        if let Some(value) = details.get(key).filter(|value| !value.is_null()) {
            object.insert(key.to_owned(), value.clone());
        }
    }
    for key in [
        "terminal",
        "result_available",
        "backgrounded",
        "required_before_final",
        "retry_after_ms",
        "next_poll_after_ms",
        "next_poll_after_unix_ms",
    ] {
        if let Some(value) = details.get(key).filter(|value| !value.is_null()) {
            object.insert(key.to_owned(), value.clone());
        }
    }
    Some(serde_json::Value::Object(object))
}

pub(super) fn agent_result_poll_tool_name(tool_name: &str) -> bool {
    matches!(tool_name, "wait_agent" | "read_agent_result")
}

fn format_tool_preview_summary(
    tool_name: &str,
    total_lines: usize,
    shown_lines: usize,
    hidden_lines: usize,
    bytes: u64,
) -> String {
    let line_label = if total_lines == 1 { "line" } else { "lines" };
    let size = format_bytes(bytes);
    if hidden_lines == 0 {
        return format!("{total_lines} {line_label} · {size}");
    }
    if tool_name == "bash" {
        return format!("last {shown_lines}/{total_lines} {line_label} · {size}");
    }
    format!("first {shown_lines}/{total_lines} {line_label} · {size}")
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1_000 {
        return format!("{bytes} B");
    }
    if bytes < 1_000_000 {
        return format!("{:.1} KB", bytes as f64 / 1_000.0);
    }
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

fn looks_like_markdown_document(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.starts_with('#')
        || trimmed.contains("\n#")
        || trimmed.contains("```")
        || trimmed.contains("\n- ")
        || trimmed.contains("\n* ")
        || trimmed.contains("\n1. ")
        || (trimmed.contains('|') && trimmed.contains("---"))
}

pub(super) fn build_model_picker_options(current: &str, remote: Vec<String>) -> Vec<String> {
    let mut options = if remote.is_empty() {
        KNOWN_MODEL_IDS
            .iter()
            .map(|model| (*model).to_owned())
            .collect::<Vec<_>>()
    } else {
        remote
    };
    let trimmed = current.trim();
    if !trimmed.is_empty() && !options.iter().any(|option| option == trimmed) {
        options.push(trimmed.to_owned());
    }
    options
}

#[cfg_attr(coverage, allow(dead_code))]
pub(super) fn non_empty_or(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub(super) fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(byte_index, _)| byte_index)
        .unwrap_or(value.len())
}

pub(super) fn persisted_root_config(root_config: &RootConfig) -> RootConfig {
    root_config.clone()
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/formatting_detail_tests.rs"]
mod tests;
