use super::*;

pub(super) struct ResultPageRequest {
    offset_chars: usize,
    max_chars: usize,
}

#[derive(Debug, Clone)]
pub(super) struct ResultPage {
    text: String,
    offset_chars: usize,
    returned_chars: usize,
    total_chars: usize,
    next_offset_chars: Option<usize>,
    truncated: bool,
}

pub(super) fn required_result_page_request_arg(args: &Value) -> Result<ResultPageRequest> {
    let offset_chars = optional_usize_arg(args, "offset_chars")?.unwrap_or(0);
    let max_chars = optional_usize_arg(args, "max_chars")?.unwrap_or(DEFAULT_RESULT_PAGE_LIMIT);
    if !(MIN_RESULT_SUMMARY_LIMIT..=MAX_RESULT_PAGE_LIMIT).contains(&max_chars) {
        return Err(anyhow!(
            "max_chars must be between {MIN_RESULT_SUMMARY_LIMIT} and {MAX_RESULT_PAGE_LIMIT}"
        ));
    }
    Ok(ResultPageRequest {
        offset_chars,
        max_chars,
    })
}

pub(super) fn optional_usize_arg(args: &Value, key: &str) -> Result<Option<usize>> {
    let Some(value) = args.get(key) else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .map(Some)
        .ok_or_else(|| anyhow!("{key} must be an integer"))
}

pub(super) fn read_agent_result_page(
    parent_session: &Session,
    result: &AgentThreadResult,
    request: ResultPageRequest,
) -> Result<ResultPage> {
    let Some(parent_path) = parent_session.store_path() else {
        return Err(anyhow!(
            "agent result page unavailable because parent session has no durable store"
        ));
    };
    let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
    let session_ref = result
        .final_answer_ref
        .as_ref()
        .map(|reference| &reference.session_ref)
        .unwrap_or(&result.session_ref);
    let child_path = session_ref.resolve(parent_dir);
    let entries = JsonlSessionStore::read_entries(&child_path).with_context(|| {
        format!(
            "failed to read child agent session {}",
            child_path.display()
        )
    })?;
    let final_text = if let Some(final_answer_ref) = result.final_answer_ref.as_ref() {
        agent_final_text_from_ref(&entries, final_answer_ref).with_context(|| {
            format!(
                "failed to read final answer from child agent session {}",
                child_path.display()
            )
        })?
    } else {
        agent_final_text_from_entries(&entries, &result.output_hash).with_context(|| {
            format!(
                "failed to read legacy final answer from child agent session {}",
                child_path.display()
            )
        })?
    };
    Ok(slice_result_page(&final_text, request))
}

pub(super) fn agent_final_text_from_ref(
    entries: &[SessionLogEntry],
    final_answer_ref: &sigil_kernel::AgentFinalAnswerRef,
) -> Result<String> {
    let message = entries.iter().find_map(|entry| {
        let SessionLogEntry::Assistant(message) = entry else {
            return None;
        };
        (message.id == final_answer_ref.message_id).then_some(message)
    });
    let Some(message) = message else {
        return Err(anyhow!(
            "child agent final answer message {} was not found",
            final_answer_ref.message_id
        ));
    };
    let content = message
        .content
        .as_ref()
        .filter(|content| !content.is_empty())
        .ok_or_else(|| anyhow!("child agent final answer message has no content"))?;
    let hash = hash_text(content);
    if hash != final_answer_ref.content_hash {
        return Err(anyhow!(
            "child agent final answer hash mismatch for message {}",
            final_answer_ref.message_id
        ));
    }
    Ok(content.clone())
}

pub(super) fn agent_final_text_from_entries(
    entries: &[SessionLogEntry],
    output_hash: &str,
) -> Result<String> {
    let mut latest_assistant_text = None;
    for entry in entries {
        let SessionLogEntry::Assistant(message) = entry else {
            continue;
        };
        let Some(content) = message
            .content
            .as_ref()
            .filter(|content| !content.is_empty())
        else {
            continue;
        };
        if hash_text(content) == output_hash {
            return Ok(content.clone());
        }
        latest_assistant_text = Some(content.clone());
    }
    latest_assistant_text
        .ok_or_else(|| anyhow!("child agent session has no assistant final answer"))
}

pub(super) fn slice_result_page(full_text: &str, request: ResultPageRequest) -> ResultPage {
    let total_chars = full_text.chars().count();
    let text = full_text
        .chars()
        .skip(request.offset_chars)
        .take(request.max_chars)
        .collect::<String>();
    let returned_chars = text.chars().count();
    let end_offset = request.offset_chars.saturating_add(returned_chars);
    let truncated = end_offset < total_chars;
    ResultPage {
        text,
        offset_chars: request.offset_chars,
        returned_chars,
        total_chars,
        next_offset_chars: truncated.then_some(end_offset),
        truncated,
    }
}

pub(super) fn agent_result_tool_result(
    call: &ToolCall,
    thread_id: &AgentThreadId,
    display_name: Option<&str>,
    result: Option<&sigil_kernel::AgentThreadResult>,
    max_summary_chars: usize,
) -> ToolResult {
    let Some(result) = result else {
        let retry_after_ms = 5_000_u64;
        let next_poll_after_unix_ms = unix_time_ms().saturating_add(retry_after_ms);
        return ToolResult::ok(
            call.id.clone(),
            call.name.clone(),
            format!("agent thread {} is still running", thread_id.as_str()),
            ToolResultMeta {
                details: json!({
                    "thread_id": thread_id.as_str(),
                    "display_name": display_name,
                    "status": "running",
                    "retry_after_ms": retry_after_ms,
                    "next_poll_after_ms": retry_after_ms,
                    "next_poll_after_unix_ms": next_poll_after_unix_ms,
                }),
                ..ToolResultMeta::default()
            },
        );
    };
    let summary = bounded_summary(&result.summary, max_summary_chars);
    let summary_truncated =
        result.summary_truncated || summary.chars().count() < result.summary.chars().count();
    let result_fetch = json!({
        "tool": READ_AGENT_RESULT_TOOL_NAME,
        "thread_id": result.thread_id.as_str(),
        "offset_chars": 0,
        "max_chars": DEFAULT_RESULT_PAGE_LIMIT,
        "max_page_chars": MAX_RESULT_PAGE_LIMIT
    });
    let payload = json!({
        "thread_id": result.thread_id.as_str(),
        "display_name": display_name,
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "summary": summary,
        "summary_truncated": summary_truncated,
        "original_summary_chars": result.original_summary_chars,
        "changed_paths": result.changed_paths,
        "artifacts": result.artifacts,
        "risks": result.risks,
        "followups": result.followups,
        "usage": result.usage,
        "output_hash": result.output_hash,
        "final_answer_ref": result.final_answer_ref.as_ref().map(|reference| json!({
            "session_ref": reference.session_ref.as_path().display().to_string(),
            "message_id": reference.message_id,
            "content_hash": reference.content_hash,
            "char_count": reference.char_count
        })),
        "truncated": summary_truncated,
        "full_result_available": !result.artifacts.is_empty(),
        "result_fetch": result_fetch
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize agent result: {error}")),
        ToolResultMeta {
            truncated: summary_truncated,
            limit_bytes: Some(max_summary_chars as u64),
            details: json!({
                "thread_id": result.thread_id.as_str(),
                "display_name": display_name,
                "status": terminal_status_label(result.status),
                "output_hash": result.output_hash,
                "has_final_answer_ref": result.final_answer_ref.is_some(),
                "summary_truncated": summary_truncated,
                "original_summary_chars": result.original_summary_chars,
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(super) fn agent_status_tool_result(
    call: &ToolCall,
    thread: &AgentThreadProjection,
) -> ToolResult {
    let result = thread.result.as_ref();
    let retry_after_ms = (!thread.status.is_terminal()).then_some(5_000_u64);
    let next_poll_after_unix_ms = retry_after_ms.map(|retry| unix_time_ms().saturating_add(retry));
    let payload = json!({
        "thread_id": thread.thread_id.as_str(),
        "display_name": thread.display_name.as_deref(),
        "status": thread_status_label(thread.status),
        "terminal": thread.status.is_terminal(),
        "reason": &thread.reason,
        "result_available": result.is_some(),
        "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
        "retry_after_ms": retry_after_ms,
        "next_poll_after_ms": retry_after_ms,
        "next_poll_after_unix_ms": next_poll_after_unix_ms,
        "next_action": if thread.status.is_terminal() {
            "use result_ref/read_args when more detail is needed"
        } else {
            "continue independent parent work; do not call wait_agent again immediately"
        },
        "result_ref": result.map(|result| json!({
            "thread_id": result.thread_id.as_str(),
            "status": terminal_status_label(result.status),
            "session_ref": result.session_ref.as_path().display().to_string(),
            "summary_truncated": result.summary_truncated,
            "original_summary_chars": result.original_summary_chars,
            "changed_paths_count": result.changed_paths.len(),
            "artifact_count": result.artifacts.len(),
            "output_hash": result.output_hash,
            "final_answer_ref": result.final_answer_ref.as_ref().map(|reference| json!({
                "session_ref": reference.session_ref.as_path().display().to_string(),
                "message_id": reference.message_id,
                "content_hash": reference.content_hash,
                "char_count": reference.char_count
            })),
            "read_tool": READ_AGENT_RESULT_TOOL_NAME,
            "read_args": {
                "thread_id": result.thread_id.as_str(),
                "offset_chars": 0,
                "max_chars": DEFAULT_RESULT_PAGE_LIMIT,
                "max_page_chars": MAX_RESULT_PAGE_LIMIT
            }
        })),
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize agent status: {error}")),
        ToolResultMeta {
            details: json!({
                "thread_id": thread.thread_id.as_str(),
                "display_name": thread.display_name.as_deref(),
                "status": thread_status_label(thread.status),
                "result_available": result.is_some(),
                "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
                "retry_after_ms": retry_after_ms,
                "next_poll_after_ms": retry_after_ms,
                "next_poll_after_unix_ms": next_poll_after_unix_ms,
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(super) fn agent_backgrounded_tool_result(
    call: &ToolCall,
    thread: &AgentThreadProjection,
) -> ToolResult {
    let retry_after_ms = 5_000_u64;
    let next_poll_after_unix_ms = unix_time_ms().saturating_add(retry_after_ms);
    let payload = json!({
        "thread_id": thread.thread_id.as_str(),
        "display_name": thread.display_name.as_deref(),
        "status": thread_status_label(thread.status),
        "terminal": false,
        "reason": &thread.reason,
        "result_available": false,
        "backgrounded": true,
        "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
        "retry_after_ms": retry_after_ms,
        "next_poll_after_ms": retry_after_ms,
        "next_poll_after_unix_ms": next_poll_after_unix_ms,
        "next_action": "continue independent parent work; use wait_agent later when a result is needed",
        "do_not_describe_as_finished": true
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize agent status: {error}")),
        ToolResultMeta {
            details: json!({
                "thread_id": thread.thread_id.as_str(),
                "display_name": thread.display_name.as_deref(),
                "status": thread_status_label(thread.status),
                "terminal": false,
                "result_available": false,
                "backgrounded": true,
                "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
                "retry_after_ms": retry_after_ms,
                "next_poll_after_ms": retry_after_ms,
                "next_poll_after_unix_ms": next_poll_after_unix_ms,
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(super) fn agent_wait_throttled_tool_result(
    call: &ToolCall,
    thread: &AgentThreadProjection,
    retry_after: Duration,
) -> ToolResult {
    let retry_after_ms = retry_after.as_millis().max(1) as u64;
    let next_poll_after_unix_ms = unix_time_ms().saturating_add(retry_after_ms);
    let payload = json!({
        "thread_id": thread.thread_id.as_str(),
        "display_name": thread.display_name.as_deref(),
        "status": thread_status_label(thread.status),
        "terminal": thread.status.is_terminal(),
        "reason": &thread.reason,
        "result_available": thread.result.is_some(),
        "retry_after_ms": retry_after_ms,
        "next_poll_after_ms": retry_after_ms,
        "next_poll_after_unix_ms": next_poll_after_unix_ms,
        "coalesced": true,
        "polling_throttled": true,
        "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
        "next_action": "wait_agent was called too soon for the same running thread; continue independent parent work and retry after retry_after_ms"
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize agent status: {error}")),
        ToolResultMeta {
            details: json!({
                "thread_id": thread.thread_id.as_str(),
                "display_name": thread.display_name.as_deref(),
                "status": thread_status_label(thread.status),
                "result_available": thread.result.is_some(),
                "retry_after_ms": retry_after_ms,
                "next_poll_after_ms": retry_after_ms,
                "next_poll_after_unix_ms": next_poll_after_unix_ms,
                "coalesced": true,
                "polling_throttled": true,
                "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(super) fn agent_result_page_tool_result(
    call: &ToolCall,
    result: &AgentThreadResult,
    page: &ResultPage,
) -> ToolResult {
    let persistent_payload = json!({
        "thread_id": result.thread_id.as_str(),
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "output_hash": result.output_hash,
        "final_answer_ref": result.final_answer_ref.as_ref().map(|reference| json!({
            "session_ref": reference.session_ref.as_path().display().to_string(),
            "message_id": reference.message_id,
            "content_hash": reference.content_hash,
            "char_count": reference.char_count
        })),
        "page": {
            "offset_chars": page.offset_chars,
            "returned_chars": page.returned_chars,
            "total_chars": page.total_chars,
            "next_offset_chars": page.next_offset_chars,
            "truncated": page.truncated,
            "text_omitted": true,
            "text_delivery": "transient_context"
        }
    });
    let transient_payload = json!({
        "thread_id": result.thread_id.as_str(),
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "output_hash": result.output_hash,
        "final_answer_ref": result.final_answer_ref.as_ref().map(|reference| json!({
            "session_ref": reference.session_ref.as_path().display().to_string(),
            "message_id": reference.message_id,
            "content_hash": reference.content_hash,
            "char_count": reference.char_count
        })),
        "page": {
            "text": page.text.as_str(),
            "offset_chars": page.offset_chars,
            "returned_chars": page.returned_chars,
            "total_chars": page.total_chars,
            "next_offset_chars": page.next_offset_chars,
            "truncated": page.truncated
        }
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&persistent_payload)
            .unwrap_or_else(|error| format!("failed to serialize agent result page: {error}")),
        ToolResultMeta {
            truncated: page.truncated,
            limit_bytes: Some(page.returned_chars as u64),
            details: json!({
                "thread_id": result.thread_id.as_str(),
                "status": terminal_status_label(result.status),
                "output_hash": result.output_hash,
                "offset_chars": page.offset_chars,
                "returned_chars": page.returned_chars,
                "total_chars": page.total_chars,
            }),
            ..ToolResultMeta::default()
        },
    )
    .with_transient_context(vec![ModelMessage::user(format!(
        "Transient read_agent_result page for tool_call_id={}:\n{}",
        call.id,
        serde_json::to_string(&transient_payload).unwrap_or_else(|error| format!(
            "failed to serialize transient agent result page: {error}"
        ))
    ))])
}

pub(super) fn agent_spawn_denied_tool_result(call: &ToolCall, reason: String) -> ToolResult {
    let Some(details) = agent_budget_denied_details(&reason) else {
        return ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::PermissionDenied,
            reason,
        );
    };
    let content = serde_json::to_string(&details)
        .unwrap_or_else(|error| format!("failed to serialize agent budget denial: {error}"));
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::PermissionDenied,
        reason,
    )
    .with_error_details(false, details.clone());
    result.content = content;
    result.metadata.details = details;
    result
}

pub(super) fn agent_budget_denied_details(reason: &str) -> Option<Value> {
    if !reason.contains("agent budget denied") && !reason.contains("agent budget exceeded") {
        return None;
    }
    Some(json!({
        "reason": reason,
        "retryable_after_slot_available": true,
        "do_not_self_complete_delegated_scope": true,
        "config_paths": agent_budget_denied_config_paths(reason),
        "next_action": "report the delegated agent could not be started; ask whether to retry after a slot is available or change the task budget instead of completing that delegated scope in the parent"
    }))
}

pub(super) fn agent_budget_denied_config_paths(reason: &str) -> Vec<&'static str> {
    let mut paths = Vec::new();
    if reason.contains("[task].max_background_threads") {
        paths.push("[task].max_background_threads");
    }
    if reason.contains("[task].max_parallel_readonly") {
        paths.push("[task].max_parallel_readonly");
    }
    if reason.contains("fan-out budget") || reason.contains("max_spawn_fanout_per_turn") {
        paths.push("[task].max_spawn_fanout_per_turn");
    }
    if reason.contains("token budget") || reason.contains("max_agent_tokens_per_task") {
        paths.push("[task].max_agent_tokens_per_task");
    }
    if paths.is_empty() {
        paths.push("[task]");
    }
    paths
}
