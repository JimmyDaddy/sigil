use std::collections::HashSet;

use sigil_kernel::{AssistantMessageKind, ControlEntry, ModelMessage, SessionLogEntry};

use super::{
    super::{
        TimelineRole,
        formatting::{
            agent_result_poll_tool_name, format_agent_thread_started_block,
            format_agent_thread_status_block, format_terminal_task_block_redacted,
            format_tool_content_block_redacted_for_restore,
        },
        worker_bridge::tool_card_replacement_key,
    },
    audit_log::{
        restored_reasoning_note, restored_tool_call_index, restored_tool_execution_content,
        restored_tool_execution_index, restored_tool_preview_snapshot_index,
        restored_tool_result_call_ids, should_render_restored_tool_execution,
    },
};

pub(super) fn restored_timeline_entries_from_session_entries(
    entries: &[SessionLogEntry],
    redactor: &sigil_kernel::SecretRedactor,
) -> Vec<crate::timeline::TimelineEntry> {
    let restored_tool_executions = restored_tool_execution_index(entries);
    let restored_tool_calls = restored_tool_call_index(entries);
    let restored_tool_previews = restored_tool_preview_snapshot_index(entries);
    let restored_tool_result_call_ids = restored_tool_result_call_ids(entries);
    let suppressed_reasoning_trace_indices = suppressed_reasoning_trace_indices(entries);
    let suppressed_assistant_preamble_indices = suppressed_assistant_preamble_indices(entries);
    let mut timeline = Vec::new();
    for (entry_index, entry) in entries.iter().enumerate() {
        match entry {
            SessionLogEntry::User(message) => {
                if let Some(content) = message.content.as_ref() {
                    timeline.push(crate::timeline::TimelineEntry {
                        role: TimelineRole::User,
                        text: content.clone(),
                    });
                }
            }
            SessionLogEntry::Assistant(message) => {
                if !suppressed_assistant_preamble_indices.contains(&entry_index)
                    && let Some(content) = message.content.as_ref()
                    && !content.is_empty()
                {
                    timeline.push(crate::timeline::TimelineEntry {
                        role: TimelineRole::Assistant,
                        text: content.clone(),
                    });
                }
            }
            SessionLogEntry::ToolResult(message) => {
                if let Some(content) = message.content.as_ref() {
                    let execution = message
                        .tool_call_id
                        .as_deref()
                        .and_then(|call_id| restored_tool_executions.get(call_id));
                    let preview = message
                        .tool_call_id
                        .as_deref()
                        .and_then(|call_id| restored_tool_previews.get(call_id));
                    let tool_call = message
                        .tool_call_id
                        .as_deref()
                        .and_then(|call_id| restored_tool_calls.get(call_id));
                    push_restored_tool_card(
                        &mut timeline,
                        format_tool_content_block_redacted_for_restore(
                            message.tool_call_id.as_deref(),
                            content,
                            execution,
                            tool_call,
                            preview,
                            redactor,
                        ),
                    );
                }
            }
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "reasoning_delta" || kind == "reasoning_trace" =>
            {
                if !suppressed_reasoning_trace_indices.contains(&entry_index)
                    && let Some(delta) = restored_reasoning_note(kind, data)
                {
                    push_restored_reasoning_timeline_entry(&mut timeline, &delta);
                }
            }
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if should_render_restored_tool_execution(
                    execution.as_ref(),
                    &restored_tool_result_call_ids,
                ) =>
            {
                let preview = restored_tool_previews.get(&execution.call_id);
                let tool_call = restored_tool_calls.get(&execution.call_id);
                push_restored_tool_card(
                    &mut timeline,
                    format_tool_content_block_redacted_for_restore(
                        Some(execution.call_id.as_str()),
                        &restored_tool_execution_content(execution.as_ref()),
                        Some(execution.as_ref()),
                        tool_call,
                        preview,
                        redactor,
                    ),
                );
            }
            SessionLogEntry::Control(ControlEntry::TerminalTask(task)) => {
                push_restored_tool_card(
                    &mut timeline,
                    format_terminal_task_block_redacted(task, redactor),
                );
            }
            SessionLogEntry::Control(ControlEntry::AgentThreadStarted(entry)) => {
                push_restored_tool_card(&mut timeline, format_agent_thread_started_block(entry));
            }
            SessionLogEntry::Control(ControlEntry::AgentThreadStatusChanged(entry)) => {
                push_restored_tool_card(&mut timeline, format_agent_thread_status_block(entry));
            }
            SessionLogEntry::Control(_) => {}
        }
    }
    timeline
}

fn push_restored_tool_card(timeline: &mut Vec<crate::timeline::TimelineEntry>, text: String) {
    let Some(current_key) = tool_card_replacement_key(&text) else {
        timeline.push(crate::timeline::TimelineEntry {
            role: TimelineRole::Tool,
            text,
        });
        return;
    };
    let mut matching_indices = timeline
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            (entry.role == TimelineRole::Tool
                && tool_card_replacement_key(&entry.text)
                    .is_some_and(|previous_key| previous_key == current_key))
            .then_some(index)
        })
        .collect::<Vec<_>>();
    let Some(keep_index) = matching_indices.first().copied() else {
        timeline.push(crate::timeline::TimelineEntry {
            role: TimelineRole::Tool,
            text,
        });
        return;
    };
    timeline[keep_index].text = text;
    matching_indices.remove(0);
    for index in matching_indices.into_iter().rev() {
        timeline.remove(index);
    }
}

pub(super) fn suppressed_reasoning_trace_indices(entries: &[SessionLogEntry]) -> HashSet<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            restored_reasoning_note_entry(entry)
                .then_some(())
                .filter(|_| reasoning_trace_is_immediately_before_agent_poll(entries, index))
                .map(|_| index)
        })
        .collect()
}

fn reasoning_trace_is_immediately_before_agent_poll(
    entries: &[SessionLogEntry],
    index: usize,
) -> bool {
    for entry in entries.iter().skip(index.saturating_add(1)) {
        if restored_reasoning_note_entry(entry) {
            continue;
        }
        return matches!(
            entry,
            SessionLogEntry::Assistant(message)
                if assistant_message_calls_suppressed_agent_poll(message)
        );
    }
    false
}

pub(super) fn suppressed_assistant_preamble_indices(entries: &[SessionLogEntry]) -> HashSet<usize> {
    let final_answer_indices = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            SessionLogEntry::Assistant(message) if assistant_message_is_final_answer(message) => {
                Some(index)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if final_answer_indices.is_empty() {
        return HashSet::new();
    }
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let SessionLogEntry::Assistant(message) = entry else {
                return None;
            };
            let has_preamble = assistant_message_is_tool_preamble(message)
                || !message.tool_calls.is_empty()
                    && message
                        .content
                        .as_ref()
                        .is_some_and(|content| !content.trim().is_empty());
            (has_preamble
                && final_answer_indices
                    .iter()
                    .any(|final_index| *final_index > index))
            .then_some(index)
        })
        .collect()
}

fn restored_reasoning_note_entry(entry: &SessionLogEntry) -> bool {
    matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::Note { kind, .. })
            if kind == "reasoning_delta" || kind == "reasoning_trace"
    )
}

fn assistant_message_is_final_answer(message: &ModelMessage) -> bool {
    if message.assistant_kind == Some(AssistantMessageKind::FinalAnswer) {
        return true;
    }
    if message.assistant_kind.is_some() {
        return false;
    }
    message.tool_calls.is_empty()
        && message
            .content
            .as_ref()
            .is_some_and(|content| !content.trim().is_empty())
}

fn assistant_message_is_tool_preamble(message: &ModelMessage) -> bool {
    message.assistant_kind == Some(AssistantMessageKind::ToolPreamble)
}

fn assistant_message_calls_suppressed_agent_poll(message: &ModelMessage) -> bool {
    message
        .tool_calls
        .iter()
        .any(|call| agent_result_poll_tool_name(call.name.as_str()))
}

pub(super) fn push_restored_reasoning_timeline_entry(
    timeline: &mut Vec<crate::timeline::TimelineEntry>,
    delta: &str,
) {
    if delta.is_empty() {
        return;
    }
    if let Some(entry) = timeline
        .last_mut()
        .filter(|entry| entry.role == TimelineRole::Thinking)
    {
        entry.text.push_str(delta);
        return;
    }
    if delta.trim().is_empty() {
        return;
    }
    timeline.push(crate::timeline::TimelineEntry {
        role: TimelineRole::Thinking,
        text: delta.to_owned(),
    });
}
