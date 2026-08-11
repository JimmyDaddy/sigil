use sigil_kernel::RunEvent;

use super::{
    run_event_helpers::notice_is_timeline_worthy,
    tool_card_lifecycle::{attach_progress_execution_id, tool_progress_result},
};
use crate::app::{
    AgentView, AppState, TimelineEntry, TimelineRole,
    agent_flow::{
        CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT, child_tool_event_scope, format_child_pending_tool_card,
    },
    formatting::{
        format_tool_progress_block_redacted_with_call, format_tool_result_block_redacted_with_call,
    },
    state::ChildToolCardOccurrence,
};

impl AppState {
    pub(super) fn handle_agent_thread_event(
        &mut self,
        thread_id: &sigil_kernel::AgentThreadId,
        event: RunEvent,
    ) {
        let AgentView::Child {
            child_task_id,
            child_session_ref,
        } = &self.agent_panel.active_view
        else {
            return;
        };
        if child_task_id != thread_id.as_str() {
            return;
        }
        let child_scope = child_tool_event_scope(thread_id.as_str(), child_session_ref);
        self.agent_panel
            .safe_child_tool_calls
            .retain(|(scope, _), _| scope == &child_scope);
        self.agent_panel
            .child_tool_progress_execution_ids
            .retain(|(scope, _), _| scope == &child_scope);
        self.agent_panel
            .child_tool_card_entry_indices
            .retain(|(scope, _), _| scope == &child_scope);
        if self.agent_panel.active_child_transcript.is_none() {
            self.reload_active_agent_child_transcript();
        }
        if self.append_live_agent_thread_event(child_scope.as_str(), event) {
            self.rerender_active_agent_child_transcript();
        }
    }

    fn append_live_agent_thread_event(&mut self, child_scope: &str, event: RunEvent) -> bool {
        match event {
            RunEvent::TextDelta(delta) => {
                self.append_live_child_delta(TimelineRole::Assistant, delta)
            }
            RunEvent::ReasoningDelta(delta) => {
                self.append_live_child_delta(TimelineRole::Thinking, delta)
            }
            RunEvent::ToolCallStarted(call) => {
                // Started may still carry provider-exact args; the pending card uses only id/name.
                let rendered = format_child_pending_tool_card(&call, None, &self.secret_redactor);
                let key = child_tool_event_key(child_scope, call.id.as_str());
                // A Started event establishes a new occurrence even when a prior failed audit with
                // the same provider call id is still awaiting its final ToolResult.
                self.agent_panel.safe_child_tool_calls.remove(&key);
                self.agent_panel
                    .child_tool_progress_execution_ids
                    .remove(&key);
                self.agent_panel.child_tool_card_entry_indices.remove(&key);
                self.push_live_child_tool_occurrence(key, rendered)
            }
            RunEvent::ToolCallCompleted(call) => {
                let key = child_tool_event_key(child_scope, call.id.as_str());
                self.agent_panel
                    .safe_child_tool_calls
                    .insert(key.clone(), call.clone());
                let rendered =
                    format_child_pending_tool_card(&call, Some(&call), &self.secret_redactor);
                self.replace_or_push_live_child_tool_card(key, rendered, false)
            }
            RunEvent::ToolResult(mut result) => {
                let key = child_tool_event_key(child_scope, result.call_id.as_str());
                if let Some(execution_id) = self
                    .agent_panel
                    .child_tool_progress_execution_ids
                    .remove(&key)
                {
                    attach_progress_execution_id(&mut result, execution_id.as_str());
                }
                let tool_call = self.agent_panel.safe_child_tool_calls.remove(&key);
                let rendered = format_tool_result_block_redacted_with_call(
                    &result,
                    tool_call.as_ref(),
                    None,
                    &self.secret_redactor,
                );
                self.replace_or_push_live_child_tool_card(key, rendered, true)
            }
            RunEvent::ToolProgress(progress) => {
                let key = child_tool_event_key(child_scope, progress.call_id.as_str());
                self.agent_panel
                    .child_tool_progress_execution_ids
                    .insert(key.clone(), progress.execution_id.as_str().to_owned());
                let result = tool_progress_result(progress);
                let tool_call = self.agent_panel.safe_child_tool_calls.get(&key);
                let rendered = format_tool_progress_block_redacted_with_call(
                    &result,
                    tool_call,
                    &self.secret_redactor,
                );
                self.replace_or_push_live_child_tool_card(key, rendered, false)
            }
            RunEvent::AssistantMessage(message) => {
                let mut changed = false;
                for call in &message.tool_calls {
                    let key = child_tool_event_key(child_scope, call.id.as_str());
                    self.agent_panel
                        .safe_child_tool_calls
                        .insert(key.clone(), call.clone());
                    let rendered =
                        format_child_pending_tool_card(call, Some(call), &self.secret_redactor);
                    changed |= self.replace_or_push_live_child_tool_card(key, rendered, false);
                }
                if message.assistant_kind == Some(sigil_kernel::AssistantMessageKind::ToolPreamble)
                {
                    return changed;
                }
                let Some(content) = message.content.filter(|content| !content.is_empty()) else {
                    return changed;
                };
                changed | self.replace_or_push_live_child_entry(TimelineRole::Assistant, content)
            }
            RunEvent::Notice(notice) => {
                if notice_is_timeline_worthy(&notice) {
                    self.push_live_child_entry(TimelineRole::Notice, notice)
                } else {
                    false
                }
            }
            RunEvent::ToolApprovalRequested { call, .. } => self.push_live_child_entry(
                TimelineRole::Notice,
                format!("Approve {} in child agent", call.name),
            ),
            RunEvent::ToolApprovalResolved {
                call_id, approved, ..
            } => self.push_live_child_entry(
                TimelineRole::Notice,
                format!(
                    "Approval {} for {}",
                    if approved { "allowed" } else { "denied" },
                    call_id
                ),
            ),
            RunEvent::ToolCallArgsDelta { .. }
            | RunEvent::Usage(_)
            | RunEvent::ContinuationState(_)
            | RunEvent::Control(_) => false,
        }
    }

    fn append_live_child_delta(&mut self, role: TimelineRole, delta: String) -> bool {
        if delta.is_empty() {
            return false;
        }
        let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        if let Some(entry) = transcript
            .timeline_entries
            .last_mut()
            .filter(|entry| entry.role == role)
        {
            if entry.text.trim().is_empty() && delta.trim().is_empty() {
                return false;
            }
            entry.text.push_str(&delta);
        } else {
            if delta.trim().is_empty() {
                return false;
            }
            transcript.total_timeline_entries = transcript
                .total_timeline_entries
                .max(transcript.timeline_entries.len())
                .saturating_add(1);
            transcript
                .timeline_entries
                .push(TimelineEntry { role, text: delta });
        }
        self.trim_live_child_transcript_to_limit();
        true
    }

    fn push_live_child_entry(&mut self, role: TimelineRole, text: String) -> bool {
        self.push_live_child_entry_with_count(role, text, true)
    }

    fn push_live_child_entry_with_count(
        &mut self,
        role: TimelineRole,
        text: String,
        is_new_logical_entry: bool,
    ) -> bool {
        let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        transcript.total_timeline_entries = transcript
            .total_timeline_entries
            .max(transcript.timeline_entries.len());
        if is_new_logical_entry {
            transcript.total_timeline_entries = transcript.total_timeline_entries.saturating_add(1);
        }
        transcript
            .timeline_entries
            .push(TimelineEntry { role, text });
        self.trim_live_child_transcript_to_limit();
        true
    }

    fn push_live_child_tool_occurrence(&mut self, key: (String, String), rendered: String) -> bool {
        let is_new_logical_entry = !self
            .agent_panel
            .child_tool_card_entry_indices
            .get(&key)
            .is_some_and(|occurrence| occurrence.is_running());
        if !self.push_live_child_entry_with_count(
            TimelineRole::Tool,
            rendered,
            is_new_logical_entry,
        ) {
            return false;
        }
        let Some(index) = self
            .agent_panel
            .active_child_transcript
            .as_ref()
            .and_then(|transcript| transcript.timeline_entries.len().checked_sub(1))
        else {
            return false;
        };
        self.agent_panel
            .child_tool_card_entry_indices
            .insert(key, ChildToolCardOccurrence::Running(Some(index)));
        true
    }

    fn replace_or_push_live_child_tool_card(
        &mut self,
        key: (String, String),
        rendered: String,
        terminal: bool,
    ) -> bool {
        let replacement_index = self
            .agent_panel
            .child_tool_card_entry_indices
            .get(&key)
            .copied()
            .and_then(ChildToolCardOccurrence::entry_index)
            .filter(|index| {
                self.agent_panel
                    .active_child_transcript
                    .as_ref()
                    .and_then(|transcript| transcript.timeline_entries.get(*index))
                    .is_some_and(|entry| child_tool_card_matches(entry, key.1.as_str()))
            })
            .or_else(|| {
                self.agent_panel
                    .active_child_transcript
                    .as_ref()
                    .and_then(|transcript| {
                        running_child_tool_card_index(&transcript.timeline_entries, key.1.as_str())
                    })
            });

        if let Some(index) = replacement_index {
            let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() else {
                return false;
            };
            let Some(entry) = transcript.timeline_entries.get_mut(index) else {
                return false;
            };
            entry.text = rendered;
            if terminal {
                self.agent_panel.child_tool_card_entry_indices.remove(&key);
            } else {
                self.agent_panel
                    .child_tool_card_entry_indices
                    .insert(key, ChildToolCardOccurrence::Running(Some(index)));
            }
            return true;
        }

        if terminal {
            let is_new_logical_entry = !self
                .agent_panel
                .child_tool_card_entry_indices
                .contains_key(&key);
            let changed = self.push_live_child_entry_with_count(
                TimelineRole::Tool,
                rendered,
                is_new_logical_entry,
            );
            self.agent_panel.child_tool_card_entry_indices.remove(&key);
            changed
        } else {
            self.push_live_child_tool_occurrence(key, rendered)
        }
    }

    fn trim_live_child_transcript_to_limit(&mut self) {
        let overflow = self
            .agent_panel
            .active_child_transcript
            .as_ref()
            .map_or(0, |transcript| {
                transcript
                    .timeline_entries
                    .len()
                    .saturating_sub(CHILD_AGENT_TRANSCRIPT_ENTRY_LIMIT)
            });
        if overflow == 0 {
            return;
        }
        if let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() {
            transcript.timeline_entries.drain(..overflow);
        }
        self.agent_panel
            .child_tool_card_entry_indices
            .values_mut()
            .for_each(|occurrence| {
                let Some(current) = occurrence.entry_index() else {
                    return;
                };
                if current < overflow {
                    occurrence.set_entry_index(None);
                } else {
                    occurrence.set_entry_index(Some(current - overflow));
                }
            });
    }

    fn replace_or_push_live_child_entry(&mut self, role: TimelineRole, text: String) -> bool {
        let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        if let Some(entry) = transcript
            .timeline_entries
            .last_mut()
            .filter(|entry| entry.role == role)
        {
            entry.text = text;
        } else {
            transcript.total_timeline_entries = transcript
                .total_timeline_entries
                .max(transcript.timeline_entries.len())
                .saturating_add(1);
            transcript
                .timeline_entries
                .push(TimelineEntry { role, text });
        }
        self.trim_live_child_transcript_to_limit();
        true
    }
}

fn running_child_tool_card_index(timeline: &[TimelineEntry], call_id: &str) -> Option<usize> {
    timeline
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, entry)| running_child_tool_card_matches(entry, call_id).then_some(index))
}

fn running_child_tool_card_matches(entry: &TimelineEntry, call_id: &str) -> bool {
    child_tool_card_matches(entry, call_id)
        && serde_json::from_str::<serde_json::Value>(&entry.text)
            .ok()
            .is_some_and(|value| {
                value.get("status").and_then(serde_json::Value::as_str) == Some("running")
            })
}

fn child_tool_card_matches(entry: &TimelineEntry, call_id: &str) -> bool {
    if entry.role != TimelineRole::Tool {
        return false;
    }
    serde_json::from_str::<serde_json::Value>(&entry.text)
        .ok()
        .is_some_and(|value| {
            value.get("call_id").and_then(serde_json::Value::as_str) == Some(call_id)
        })
}

fn child_tool_event_key(child_scope: &str, call_id: &str) -> (String, String) {
    (child_scope.to_owned(), call_id.to_owned())
}
