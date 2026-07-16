use sigil_kernel::RunEvent;

use super::{
    run_event_helpers::notice_is_timeline_worthy, tool_card_lifecycle::tool_progress_summary,
};
use crate::app::{AgentView, AppState, TimelineEntry, TimelineRole};

impl AppState {
    pub(super) fn handle_agent_thread_event(
        &mut self,
        thread_id: &sigil_kernel::AgentThreadId,
        event: RunEvent,
    ) {
        let AgentView::Child { child_task_id, .. } = &self.agent_panel.active_view else {
            return;
        };
        if child_task_id != thread_id.as_str() {
            return;
        }
        if self.agent_panel.active_child_transcript.is_none() {
            self.reload_active_agent_child_transcript();
        }
        if self.append_live_agent_thread_event(event) {
            self.rerender_active_agent_child_transcript();
        }
    }

    fn append_live_agent_thread_event(&mut self, event: RunEvent) -> bool {
        match event {
            RunEvent::TextDelta(delta) => {
                self.append_live_child_delta(TimelineRole::Assistant, delta)
            }
            RunEvent::ReasoningDelta(delta) => {
                self.append_live_child_delta(TimelineRole::Thinking, delta)
            }
            RunEvent::ToolCallStarted(call) => {
                self.push_live_child_entry(TimelineRole::Tool, format!("Started {}", call.name))
            }
            RunEvent::ToolCallCompleted(call) => {
                self.push_live_child_entry(TimelineRole::Tool, format!("Completed {}", call.name))
            }
            RunEvent::ToolResult(result) => {
                self.push_live_child_entry(TimelineRole::Tool, result.content)
            }
            RunEvent::ToolProgress(progress) => {
                self.push_live_child_entry(TimelineRole::Tool, tool_progress_summary(&progress))
            }
            RunEvent::AssistantMessage(message) => {
                if message.assistant_kind == Some(sigil_kernel::AssistantMessageKind::ToolPreamble)
                {
                    return false;
                }
                let Some(content) = message.content.filter(|content| !content.is_empty()) else {
                    return false;
                };
                self.replace_or_push_live_child_entry(TimelineRole::Assistant, content)
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
            transcript
                .timeline_entries
                .push(TimelineEntry { role, text: delta });
            transcript.total_timeline_entries = transcript
                .total_timeline_entries
                .max(transcript.timeline_entries.len());
        }
        true
    }

    fn push_live_child_entry(&mut self, role: TimelineRole, text: String) -> bool {
        let Some(transcript) = self.agent_panel.active_child_transcript.as_mut() else {
            return false;
        };
        transcript.load_error = None;
        transcript
            .timeline_entries
            .push(TimelineEntry { role, text });
        transcript.total_timeline_entries = transcript
            .total_timeline_entries
            .max(transcript.timeline_entries.len());
        true
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
            transcript
                .timeline_entries
                .push(TimelineEntry { role, text });
            transcript.total_timeline_entries = transcript
                .total_timeline_entries
                .max(transcript.timeline_entries.len());
        }
        true
    }
}
