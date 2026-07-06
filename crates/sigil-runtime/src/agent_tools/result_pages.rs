use super::*;

pub(super) struct ResultPageRequest {
    offset_chars: usize,
    requested_max_chars: Option<usize>,
    max_chars: usize,
    max_chars_clamped: bool,
}

#[derive(Debug, Clone)]
pub(super) struct ResultPage {
    pub(super) text: String,
    pub(super) offset_chars: usize,
    pub(super) returned_chars: usize,
    pub(super) total_chars: usize,
    pub(super) next_offset_chars: Option<usize>,
    pub(super) truncated: bool,
    pub(super) requested_max_chars: Option<usize>,
    pub(super) max_chars: usize,
    pub(super) max_chars_clamped: bool,
}

pub(super) fn required_result_page_request_arg(args: &Value) -> Result<ResultPageRequest> {
    let offset_chars = optional_usize_arg(args, "offset_chars")?.unwrap_or(0);
    let requested_max_chars = optional_usize_arg(args, "max_chars")?;
    let max_chars = requested_max_chars
        .unwrap_or(DEFAULT_RESULT_PAGE_LIMIT)
        .clamp(MIN_RESULT_SUMMARY_LIMIT, MAX_RESULT_PAGE_LIMIT);
    let max_chars_clamped = requested_max_chars.is_some_and(|requested| requested != max_chars);
    Ok(ResultPageRequest {
        offset_chars,
        requested_max_chars,
        max_chars,
        max_chars_clamped,
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
        return Ok(slice_result_page(&result.summary, request));
    };
    let parent_dir = parent_path.parent().unwrap_or_else(|| Path::new("."));
    let final_answer_ref = result
        .final_answer_ref
        .as_ref()
        .ok_or_else(|| anyhow!("child agent result is missing final_answer_ref"))?;
    let session_ref = &final_answer_ref.session_ref;
    let child_path = session_ref.resolve(parent_dir);
    let entries = JsonlSessionStore::read_entries(&child_path).with_context(|| {
        format!(
            "failed to read child agent session {}",
            child_path.display()
        )
    })?;
    let final_text = agent_final_text_from_ref(&entries, final_answer_ref).with_context(|| {
        format!(
            "failed to read final answer from child agent session {}",
            child_path.display()
        )
    })?;
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
        requested_max_chars: request.requested_max_chars,
        max_chars: request.max_chars,
        max_chars_clamped: request.max_chars_clamped,
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
        let retry_after_ms = WAIT_AGENT_RUNNING_RETRY_AFTER_MS;
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
    let read_args = json!({
        "thread_id": result.thread_id.as_str(),
        "offset_chars": 0,
        "max_chars": MAX_RESULT_PAGE_LIMIT
    });
    let result_fetch = json!({
        "tool": READ_AGENT_RESULT_TOOL_NAME,
        "args": read_args,
        "max_page_chars": MAX_RESULT_PAGE_LIMIT,
        "next_action": "call read_agent_result with result_fetch.args exactly; do not estimate max_chars from char_count"
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
    let retry_after_ms =
        (!thread.status.is_terminal()).then_some(WAIT_AGENT_RUNNING_RETRY_AFTER_MS);
    let next_poll_after_unix_ms = retry_after_ms.map(|retry| unix_time_ms().saturating_add(retry));
    let wait_available = thread.status != AgentThreadStatus::Unavailable;
    let polling_recommended = retry_after_ms.is_some() && wait_available;
    let next_action = if thread.status == AgentThreadStatus::Unavailable && result.is_none() {
        "report that this agent result is unavailable in the current process; do not call wait_agent again for this thread"
    } else if thread.status.is_terminal() && result.is_some() {
        "use result_ref/read_args when more detail is needed"
    } else if thread.status.is_terminal() {
        "report this terminal agent status; no result page is available and wait_agent should not be called again"
    } else {
        "continue only non-overlapping parent work; do not call wait_agent again until retry_after_ms; wait before the final answer"
    };
    let payload = json!({
        "thread_id": thread.thread_id.as_str(),
        "display_name": thread.display_name.as_deref(),
        "status": thread_status_label(thread.status),
        "terminal": thread.status.is_terminal(),
        "reason": &thread.reason,
        "result_available": result.is_some(),
        "wait_available": wait_available,
        "polling_recommended": polling_recommended,
        "rerun_not_needed": thread.status == AgentThreadStatus::Unavailable && result.is_none(),
        "coalescing_key": format!("wait_agent:{}", thread.thread_id.as_str()),
        "retry_after_ms": retry_after_ms,
        "next_poll_after_ms": retry_after_ms,
        "next_poll_after_unix_ms": next_poll_after_unix_ms,
        "next_action": next_action,
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
                "max_chars": MAX_RESULT_PAGE_LIMIT
            },
            "max_page_chars": MAX_RESULT_PAGE_LIMIT,
            "next_action": "call read_agent_result with result_ref.read_args exactly; do not estimate max_chars from char_count"
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
                "wait_available": wait_available,
                "polling_recommended": polling_recommended,
                "rerun_not_needed": thread.status == AgentThreadStatus::Unavailable && result.is_none(),
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
        "next_action": "wait_agent was called too soon for the same running thread; continue only non-overlapping parent work and retry after retry_after_ms before the final answer"
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
    let next_read_args = page.next_offset_chars.map(|offset_chars| {
        json!({
            "thread_id": result.thread_id.as_str(),
            "offset_chars": offset_chars,
            "max_chars": MAX_RESULT_PAGE_LIMIT,
        })
    });
    let next_action = if page.truncated {
        "call read_agent_result with next_read_args to read the next page; do not increase max_chars"
    } else {
        "this child result page reaches the end; do not call read_agent_result again for this result"
    };
    let request = json!({
        "offset_chars": page.offset_chars,
        "requested_max_chars": page.requested_max_chars,
        "max_chars": page.max_chars,
        "max_chars_clamped": page.max_chars_clamped,
        "min_page_chars": MIN_RESULT_SUMMARY_LIMIT,
        "max_page_chars": MAX_RESULT_PAGE_LIMIT,
    });
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
        },
        "request": request.clone(),
        "next_read_args": next_read_args.clone(),
        "next_action": next_action,
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
        },
        "request": request,
        "next_read_args": next_read_args,
        "next_action": next_action,
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
                "requested_max_chars": page.requested_max_chars,
                "max_chars": page.max_chars,
                "max_chars_clamped": page.max_chars_clamped,
                "returned_chars": page.returned_chars,
                "total_chars": page.total_chars,
                "next_offset_chars": page.next_offset_chars,
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

pub(super) fn agent_result_already_delivered_tool_result(
    call: &ToolCall,
    result: &AgentThreadResult,
    delivered: &AgentThreadResultDeliveredEntry,
) -> ToolResult {
    let payload = json!({
        "thread_id": result.thread_id.as_str(),
        "status": terminal_status_label(result.status),
        "session_ref": result.session_ref.as_path().display().to_string(),
        "output_hash": result.output_hash,
        "already_delivered": true,
        "rerun_not_needed": true,
        "previous_delivery": {
            "call_id": delivered.call_id,
            "offset_chars": delivered.offset_chars,
            "returned_chars": delivered.returned_chars,
            "total_chars": delivered.total_chars,
            "truncated": delivered.truncated,
        },
        "next_action": "Use the previously delivered child result already in context; do not call read_agent_result again only to re-read the same full result."
    });
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        serde_json::to_string(&payload)
            .unwrap_or_else(|error| format!("failed to serialize delivered agent result: {error}")),
        ToolResultMeta {
            details: json!({
                "thread_id": result.thread_id.as_str(),
                "status": terminal_status_label(result.status),
                "output_hash": result.output_hash,
                "already_delivered": true,
                "previous_call_id": delivered.call_id,
                "rerun_not_needed": true,
            }),
            ..ToolResultMeta::default()
        },
    )
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
    if reason.contains("[task].max_subagents") || reason.contains("agent thread budget") {
        paths.push("[task].max_subagents");
    }
    if paths.is_empty() {
        paths.push("[task]");
    }
    paths
}
