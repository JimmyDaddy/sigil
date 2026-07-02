use super::*;

pub(super) fn apply_usage_control_entry(stats: &mut SessionStats, control: &ControlEntry) {
    match control {
        ControlEntry::UsageSnapshot(usage) => stats.apply_usage(usage),
        ControlEntry::CompactionApplied(_) => stats.last_prompt_tokens = 0,
        _ => {}
    }
}

pub(super) fn compaction_summary_message(record: &CompactionRecord) -> ModelMessage {
    let digest = Sha256::digest(
        format!(
            "{}\n{}\n{}",
            record.summary, record.compacted_message_count, record.retained_tail_message_count
        )
        .as_bytes(),
    );
    ModelMessage {
        id: format!("compaction:{digest:x}"),
        role: crate::MessageRole::Assistant,
        content: Some(record.summary.clone()),
        tool_calls: Vec::new(),
        tool_call_id: None,
        assistant_kind: Some(crate::AssistantMessageKind::Progress),
    }
}

pub(super) fn projected_messages_with_record(
    raw_messages: &[ModelMessage],
    record: &CompactionRecord,
) -> Vec<ModelMessage> {
    let mut projected = vec![compaction_summary_message(record)];
    if record.compacted_message_count < raw_messages.len() {
        projected.extend(
            raw_messages[record.compacted_message_count..]
                .iter()
                .cloned(),
        );
    }
    projected
}

pub(super) fn repair_orphan_tool_results(messages: &[ModelMessage]) -> Vec<ModelMessage> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut index = 0usize;

    while index < messages.len() {
        let message = &messages[index];
        repaired.push(message.clone());

        if !matches!(message.role, crate::MessageRole::Assistant) || message.tool_calls.is_empty() {
            index += 1;
            continue;
        }

        index += 1;
        let mut satisfied_call_ids = Vec::new();
        while index < messages.len() && matches!(messages[index].role, crate::MessageRole::Tool) {
            if let Some(tool_call_id) = &messages[index].tool_call_id
                && message
                    .tool_calls
                    .iter()
                    .any(|call| call.id == *tool_call_id)
            {
                satisfied_call_ids.push(tool_call_id.clone());
            }
            repaired.push(messages[index].clone());
            index += 1;
        }

        for call in &message.tool_calls {
            if !satisfied_call_ids.iter().any(|call_id| call_id == &call.id) {
                repaired.push(synthetic_orphan_tool_result(call));
            }
        }
    }

    repaired
}

pub(super) fn synthetic_orphan_tool_result(call: &crate::ToolCall) -> ModelMessage {
    let result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Interrupted,
        format!(
            "tool call {} did not return a result before the previous run stopped; retry the tool call with valid arguments if it is still needed",
            call.name
        ),
    );
    let mut message = result.to_model_message();
    message.id = format!("local_repair:missing_tool_result:{}", call.id);
    message
}

pub(super) fn interrupted_tool_executions(entries: &[SessionLogEntry]) -> Vec<ToolExecutionEntry> {
    let mut open_executions = HashMap::<String, ToolExecutionEntry>::new();
    for entry in entries {
        let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
            continue;
        };
        match execution.status {
            ToolExecutionStatus::Started => {
                open_executions.insert(execution.call_id.clone(), execution.as_ref().clone());
            }
            ToolExecutionStatus::Completed
            | ToolExecutionStatus::Failed
            | ToolExecutionStatus::Cancelled
            | ToolExecutionStatus::Interrupted => {
                open_executions.remove(&execution.call_id);
            }
        }
    }

    open_executions
        .into_values()
        .map(|mut execution| {
            execution.status = ToolExecutionStatus::Interrupted;
            execution.duration_ms = None;
            execution.changed_files = Vec::new();
            execution.metadata.changed_files = Vec::new();
            execution.error = Some(ToolError {
                kind: ToolErrorKind::Interrupted,
                message: "tool execution was interrupted before a completion record was written"
                    .to_owned(),
                retryable: true,
                details: serde_json::Value::Null,
            });
            execution.model_content_hash = None;
            execution
        })
        .collect()
}

pub(super) fn interrupted_tool_execution_profiles(
    entries: &[SessionLogEntry],
) -> Vec<ExecutionMutationProfile> {
    entries
        .iter()
        .filter_map(|entry| {
            let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
                return None;
            };
            if execution.status != ToolExecutionStatus::Interrupted {
                return None;
            }
            execution
                .metadata
                .details
                .get("execution_mutation_profile")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        })
        .collect()
}

pub fn latest_compaction_record(entries: &[SessionLogEntry]) -> Option<CompactionRecord> {
    entries.iter().rev().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::CompactionApplied(record)) => Some(record.clone()),
        _ => None,
    })
}

pub(super) fn latest_task_memory_workspace_snapshot_id(
    records: &[SessionStreamRecord],
) -> Result<Option<crate::WorkspaceSnapshotId>> {
    for record in records.iter().rev() {
        match record {
            SessionStreamRecord::Stored(event)
                if event.event_kind() == Some(DurableEventType::MutationCommitted) =>
            {
                let committed: crate::MutationCommitted =
                    serde_json::from_value(event.payload.clone())
                        .context("failed to decode mutation commit for compaction task memory")?;
                return Ok(Some(committed.workspace_snapshot_id));
            }
            SessionStreamRecord::Stored(event) => {
                if let Some(SessionLogEntry::Control(ControlEntry::VerificationRecorded(
                    verification,
                ))) = session_entry_from_stored_event(event)?
                {
                    return Ok(Some(verification.receipt.binding.workspace_snapshot_id));
                }
            }
            SessionStreamRecord::Legacy { entry, .. } => {
                if let SessionLogEntry::Control(ControlEntry::VerificationRecorded(verification)) =
                    entry.as_ref()
                {
                    return Ok(Some(
                        verification.receipt.binding.workspace_snapshot_id.clone(),
                    ));
                }
            }
        }
    }
    Ok(None)
}

pub fn session_stats_from_entries(entries: &[SessionLogEntry]) -> SessionStats {
    let mut stats = SessionStats::default();
    for entry in entries {
        match entry {
            SessionLogEntry::Control(control) => apply_usage_control_entry(&mut stats, control),
            SessionLogEntry::User(_)
            | SessionLogEntry::Assistant(_)
            | SessionLogEntry::ToolResult(_) => {}
        }
    }
    stats
}

pub(super) fn compaction_boundary(
    messages: &[ModelMessage],
    requested_tail_messages: usize,
) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let tail_messages = requested_tail_messages.max(1);
    let mut boundary = messages.len().saturating_sub(tail_messages);
    while boundary > 0
        && (matches!(messages[boundary].role, crate::MessageRole::Tool)
            || !messages[boundary - 1].tool_calls.is_empty()
            || matches!(messages[boundary - 1].role, crate::MessageRole::Tool))
    {
        if !messages[boundary - 1].tool_calls.is_empty() {
            boundary -= 1;
            break;
        }
        boundary -= 1;
    }
    boundary
}

pub(super) fn summarize_messages(messages: &[ModelMessage]) -> String {
    let mut lines = vec![format!(
        "Compacted {} earlier messages into a stable local summary.",
        messages.len()
    )];

    for (index, message) in messages.iter().enumerate() {
        let label = match message.role {
            crate::MessageRole::System => "system",
            crate::MessageRole::User => "user",
            crate::MessageRole::Assistant => "assistant",
            crate::MessageRole::Tool => "tool",
        };
        if !message.tool_calls.is_empty() {
            let names = message
                .tool_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let content = message.content.as_deref().unwrap_or_default();
            let truncated = truncate_stable(content, 160);
            if !truncated.is_empty() {
                lines.push(format!(
                    "{:02}. {} {} tool_calls [{}]",
                    index + 1,
                    label,
                    truncated,
                    names
                ));
                continue;
            }
            lines.push(format!(
                "{:02}. {} tool_calls [{}]",
                index + 1,
                label,
                names
            ));
            continue;
        }

        let content = message.content.clone().unwrap_or_default();
        let truncated = truncate_stable(&content, 160);
        if matches!(message.role, crate::MessageRole::Tool) {
            let tool_call_id = message.tool_call_id.as_deref().unwrap_or("unknown");
            lines.push(format!(
                "{:02}. {} {} => {}",
                index + 1,
                label,
                tool_call_id,
                truncated
            ));
        } else {
            lines.push(format!("{:02}. {} {}", index + 1, label, truncated));
        }
    }

    lines.join("\n")
}

pub(super) fn truncate_stable(content: &str, max_chars: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = normalized.chars().count();
    if char_count <= max_chars {
        return normalized;
    }
    let truncated = normalized.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

pub(super) fn stable_json_hash(value: &serde_json::Value) -> String {
    let serialized =
        serde_json::to_string(value).unwrap_or_else(|_| "<unserializable-json>".to_owned());
    stable_text_hash(&serialized)
}

pub(super) fn stable_text_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

pub(super) fn json_object_keys(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

pub(super) fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(values) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut strings = values
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    strings.sort();
    strings
}

pub(super) fn json_top_level_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

pub(super) fn session_identity_from_entries(
    entries: &[SessionLogEntry],
) -> Option<(String, String)> {
    entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::SessionIdentity {
            provider_name,
            model_name,
        }) => Some((provider_name.clone(), model_name.clone())),
        SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
            Some((snapshot.provider_name.clone(), snapshot.model_name.clone()))
        }
        _ => None,
    })
}
