use std::time::{SystemTime, UNIX_EPOCH};

use ratatui::text::Line;
use termquill_kernel::{ReasoningEffort, RootConfig, ToolResult, ToolResultMeta};

use crate::slash::KNOWN_MODEL_IDS;

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
    let min = if total_width < 72 { 16 } else { 24 };
    let max = if total_width < 72 { 24 } else { 42 };
    ((total_width * 30) / 100).clamp(min, max)
}

pub(super) fn normalize_runtime_model(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = match trimmed.to_ascii_lowercase().as_str() {
        "flash" | "v4-flash" => "deepseek-v4-flash".to_owned(),
        "pro" | "v4-pro" => "deepseek-v4-pro".to_owned(),
        _ => trimmed.to_owned(),
    };
    Some(normalized)
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

pub(super) fn format_tool_result_block(result: &ToolResult) -> String {
    format_tool_preview_payload(
        Some(result.call_id.as_str()),
        result.tool_name.as_str(),
        if result.is_error { "error" } else { "ok" },
        &result.content,
        Some(&result.metadata),
    )
}

pub(super) fn format_tool_content_block(content: &str) -> String {
    format_tool_preview_payload(None, "tool_result", "ok", content, None)
}

fn format_tool_preview_payload(
    call_id: Option<&str>,
    tool_name: &str,
    status: &str,
    content: &str,
    metadata: Option<&ToolResultMeta>,
) -> String {
    let preview_value = tool_preview_value(content);
    let (preview_kind, preview_source) =
        tool_preview_source(tool_name, content, preview_value.as_ref());
    let all_lines = if preview_source.is_empty() {
        Vec::new()
    } else {
        preview_source
            .lines()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let total_lines = all_lines.len();
    let preview_lines = select_tool_preview_lines(tool_name, &all_lines);
    let hidden_lines = total_lines.saturating_sub(preview_lines.len());
    let bytes = metadata
        .and_then(|value| value.bytes)
        .unwrap_or(content.len() as u64);
    let metadata_line = metadata
        .and_then(render_tool_metadata_summary)
        .filter(|value| !value.is_empty());

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
    object.insert(
        "preview_kind".to_owned(),
        serde_json::Value::String(preview_kind.to_owned()),
    );
    object.insert(
        "summary".to_owned(),
        serde_json::Value::String(format_tool_preview_summary(
            tool_name,
            total_lines,
            preview_lines.len(),
            hidden_lines,
            bytes,
        )),
    );
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
    if let Some(metadata_line) = metadata_line {
        object.insert(
            "metadata_line".to_owned(),
            serde_json::Value::String(metadata_line),
        );
    }
    if let Some(metadata) = metadata {
        object.insert(
            "metadata".to_owned(),
            serde_json::to_value(metadata).unwrap_or(serde_json::Value::Null),
        );
    }
    if let Some(preview_value) = preview_value {
        object.insert(
            "preview_value".to_owned(),
            compact_preview_value(&preview_value, 0),
        );
    }
    serde_json::to_string(&serde_json::Value::Object(object)).unwrap_or_else(|_| content.to_owned())
}

fn parse_tool_content_value(content: &str) -> serde_json::Value {
    serde_json::from_str(content).unwrap_or_else(|_| serde_json::Value::String(content.to_owned()))
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
) -> (&'static str, String) {
    if let Some(value) = preview_value {
        let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| content.to_owned());
        return ("json", pretty);
    }
    if tool_name == "read_file" || looks_like_markdown_document(content) {
        return ("markdown", content.to_owned());
    }
    ("text", content.to_owned())
}

fn compact_preview_value(value: &serde_json::Value, depth: usize) -> serde_json::Value {
    const MAX_DEPTH: usize = 3;
    const MAX_ITEMS: usize = 10;
    const MAX_STRING_CHARS: usize = 160;

    match value {
        serde_json::Value::Array(items) => {
            if depth >= MAX_DEPTH {
                return serde_json::Value::String(format!("… {} items", items.len()));
            }
            let limit = items.len().min(MAX_ITEMS);
            let mut compacted = items
                .iter()
                .take(limit)
                .map(|item| compact_preview_value(item, depth + 1))
                .collect::<Vec<_>>();
            if items.len() > limit {
                compacted.push(serde_json::Value::String(format!(
                    "… {} more items",
                    items.len() - limit
                )));
            }
            serde_json::Value::Array(compacted)
        }
        serde_json::Value::Object(object) => {
            if depth >= MAX_DEPTH {
                return serde_json::Value::String(format!("… {} keys", object.len()));
            }
            let limit = object.len().min(MAX_ITEMS);
            let mut compacted = serde_json::Map::new();
            for (key, nested) in object.iter().take(limit) {
                compacted.insert(key.clone(), compact_preview_value(nested, depth + 1));
            }
            if object.len() > limit {
                compacted.insert(
                    "…".to_owned(),
                    serde_json::Value::String(format!("{} more keys", object.len() - limit)),
                );
            }
            serde_json::Value::Object(compacted)
        }
        serde_json::Value::String(text) => {
            let truncated = text.chars().take(MAX_STRING_CHARS).collect::<String>();
            if text.chars().count() > MAX_STRING_CHARS {
                serde_json::Value::String(format!("{truncated}..."))
            } else {
                serde_json::Value::String(truncated)
            }
        }
        _ => value.clone(),
    }
}

fn select_tool_preview_lines(tool_name: &str, lines: &[String]) -> Vec<String> {
    let limit = tool_preview_limit(tool_name);
    if lines.len() <= limit {
        return lines.to_vec();
    }
    if tool_name == "bash" {
        return lines[lines.len().saturating_sub(limit)..].to_vec();
    }
    lines[..limit].to_vec()
}

fn tool_preview_limit(tool_name: &str) -> usize {
    match tool_name {
        "bash" => 16,
        "read_file" => 18,
        "grep" | "glob" | "ls" => 14,
        _ => 12,
    }
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

fn render_tool_metadata_summary(metadata: &ToolResultMeta) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(exit_code) = metadata.exit_code {
        parts.push(format!("exit={exit_code}"));
    }
    if let Some(bytes) = metadata.bytes {
        parts.push(format!("bytes={bytes}"));
    }
    if metadata.truncated {
        parts.push("truncated".to_owned());
    }
    if !metadata.changed_files.is_empty() {
        parts.push(format!("files={}", metadata.changed_files.len()));
    }
    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" · "))
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
