use sigil_kernel::{ToolProgressEvent, ToolResult, ToolResultMeta};

use super::super::{TimelineEntry, TimelineRole, formatting::agent_result_poll_tool_name};

pub(super) fn agent_tool_name(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "wait_agent"
            | "read_agent_result"
            | "list_agents"
            | "cancel_agent"
            | "message_agent"
            | "close_agent"
    )
}

pub(super) fn suppress_reasoning_before_tool_call(name: &str) -> bool {
    agent_result_poll_tool_name(name)
}

pub(super) fn tool_card_replacement_indices(
    timeline: &[TimelineEntry],
    rendered: &str,
) -> Option<Vec<usize>> {
    let current_key = tool_card_replacement_key(rendered)?;
    const RECENT_TOOL_CARD_SCAN: usize = 96;
    let start_index = timeline.len().saturating_sub(RECENT_TOOL_CARD_SCAN);
    let indices = timeline
        .iter()
        .enumerate()
        .skip(start_index)
        .filter_map(|(index, previous)| {
            if previous.role != TimelineRole::Tool {
                return None;
            }
            let previous_key = tool_card_replacement_key(&previous.text)?;
            (previous_key == current_key).then_some(index)
        })
        .collect::<Vec<_>>();
    (!indices.is_empty()).then_some(indices)
}

pub(in crate::app) fn tool_card_replacement_key(text: &str) -> Option<String> {
    terminal_task_key_from_tool_block(text)
        .or_else(|| execution_key_from_tool_block(text))
        .or_else(|| agent_thread_key_from_tool_block(text))
}

pub(super) fn tool_progress_result(progress: ToolProgressEvent) -> ToolResult {
    let content = progress
        .output_preview
        .clone()
        .filter(|preview| !preview.is_empty())
        .unwrap_or_else(|| tool_progress_summary(&progress));
    let details = tool_progress_details(progress.execution_id.as_str(), progress.details);
    ToolResult::ok(
        progress.call_id,
        progress.tool_name,
        content,
        ToolResultMeta {
            bytes: progress.total_bytes,
            total_bytes: progress.total_bytes,
            returned_bytes: progress
                .output_preview
                .as_ref()
                .map(|preview| preview.len() as u64),
            returned_lines: progress
                .output_preview
                .as_ref()
                .map(|preview| preview.lines().count() as u64),
            details,
            ..ToolResultMeta::default()
        },
    )
}

pub(super) fn tool_progress_summary(progress: &ToolProgressEvent) -> String {
    progress.message.clone().unwrap_or_else(|| {
        format!(
            "{} {}",
            progress.tool_name,
            progress.status.replace('_', " ")
        )
    })
}

pub(super) fn terminal_task_key_from_tool_block(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let tool_name = value.get("tool_name")?.as_str()?;
    if !matches!(
        tool_name,
        "terminal_task" | "terminal_start" | "terminal_read" | "terminal_cancel"
    ) {
        return None;
    }
    let details = value.get("metadata")?.get("details")?;
    let terminal_task = details.get("terminal_task").unwrap_or(details);
    terminal_task
        .get("status")
        .and_then(serde_json::Value::as_str)?;
    let task_id = terminal_task
        .get("task_id")
        .and_then(serde_json::Value::as_str)?
        .trim();
    (!task_id.is_empty()).then(|| format!("terminal_task:{task_id}"))
}

pub(super) fn execution_key_from_tool_block(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let execution_id = value
        .get("metadata")?
        .get("details")?
        .get("execution_id")
        .and_then(serde_json::Value::as_str)?
        .trim();
    (!execution_id.is_empty()).then(|| format!("tool_execution:{execution_id}"))
}

pub(super) fn agent_thread_key_from_tool_block(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    let tool_name = value.get("tool_name")?.as_str()?;
    if !matches!(tool_name, "spawn_agent" | "wait_agent") {
        return None;
    }
    let thread_id = value
        .get("preview_value")
        .and_then(|preview| preview.get("thread_id"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    (!thread_id.is_empty()).then(|| format!("agent_thread:{thread_id}"))
}

pub(super) fn tool_progress_details(
    execution_id: &str,
    details: serde_json::Value,
) -> serde_json::Value {
    match details {
        serde_json::Value::Object(mut object) => {
            object
                .entry("execution_id".to_owned())
                .or_insert_with(|| serde_json::Value::String(execution_id.to_owned()));
            serde_json::Value::Object(object)
        }
        other => serde_json::json!({
            "execution_id": execution_id,
            "progress_details": other,
        }),
    }
}

pub(super) fn wait_agent_pending_replacement_indices(
    timeline: &[TimelineEntry],
    result: &ToolResult,
    rendered: &str,
) -> Option<Vec<usize>> {
    let current_key = wait_agent_pending_key_from_result(result, rendered)?;
    const RECENT_PENDING_WAIT_AGENT_SCAN: usize = 64;
    let start_index = timeline
        .len()
        .saturating_sub(RECENT_PENDING_WAIT_AGENT_SCAN);
    let indices = timeline
        .iter()
        .enumerate()
        .skip(start_index)
        .filter_map(|(index, previous)| {
            (previous.role == TimelineRole::Tool
                && wait_agent_pending_key_from_tool_block(&previous.text)
                    .is_some_and(|previous_key| previous_key == current_key))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    (!indices.is_empty()).then_some(indices)
}

pub(super) fn wait_agent_pending_key_from_result(
    result: &ToolResult,
    rendered: &str,
) -> Option<String> {
    if result.tool_name != "wait_agent" || result.is_error() {
        return None;
    }
    wait_agent_pending_key_from_tool_block(rendered)
}

pub(super) fn wait_agent_pending_key_from_tool_block(text: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(text).ok()?;
    if value.get("tool_name")?.as_str()? != "wait_agent" {
        return None;
    }
    if value.get("status").and_then(serde_json::Value::as_str) != Some("ok") {
        return None;
    }
    let preview = value.get("preview_value")?;
    if preview
        .get("terminal")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    preview.get("retry_after_ms")?;
    preview
        .get("coalescing_key")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            preview
                .get("thread_id")
                .and_then(serde_json::Value::as_str)
                .map(|thread_id| format!("wait_agent:{thread_id}"))
        })
}
