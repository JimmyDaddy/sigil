use std::collections::{BTreeMap, BTreeSet};

use sigil_application::{LiveRunUpdate, LiveRunUpdateKind};

use super::{AppState, RunPhase, TimelineRole};

#[derive(Debug, Default)]
pub(super) struct LivePreviewState {
    reader: Option<sigil_runtime::RuntimeLivePreviewReader>,
    session_id: String,
    run_id: String,
    attempt_id: Option<String>,
    latest_revision: u64,
    revisions: BTreeMap<String, u64>,
    retired: BTreeSet<String>,
    retired_arguments: BTreeSet<String>,
    slots: BTreeMap<String, usize>,
    durable_sequence: u64,
    pending: BTreeMap<String, LiveRunUpdate>,
    execution_pending: BTreeMap<String, LiveRunUpdate>,
    execution_revisions: BTreeMap<String, u64>,
    execution_calls: BTreeMap<String, String>,
}

impl AppState {
    pub(super) fn attach_live_preview(&mut self, source: sigil_runtime::RuntimeLivePreviewSource) {
        if source.session_id() != self.session_id || source.is_terminal() {
            return;
        }
        self.live_preview = LivePreviewState {
            session_id: source.session_id().to_owned(),
            run_id: source.run_id().to_owned(),
            reader: Some(source.reader()),
            ..LivePreviewState::default()
        };
    }

    pub(super) fn accept_live_durable_frontier(
        &mut self,
        session_id: &str,
        run_id: &str,
        sequence: u64,
    ) {
        if session_id == self.live_preview.session_id && run_id == self.live_preview.run_id {
            self.live_preview.durable_sequence = self.live_preview.durable_sequence.max(sequence);
        }
    }

    pub(crate) fn has_live_preview_work(&self) -> bool {
        self.live_preview
            .reader
            .as_ref()
            .is_some_and(|reader| !reader.is_terminal())
    }

    pub(super) fn clear_live_preview(&mut self) {
        self.live_preview = LivePreviewState::default();
    }

    pub(super) fn discard_live_attempt(&mut self) {
        self.live_preview
            .retired
            .extend(self.live_preview.revisions.keys().cloned());
        self.live_preview.pending.clear();
        self.live_preview.slots.clear();
    }

    pub(super) fn poll_live_preview(&mut self) -> bool {
        if self.live_preview.session_id != self.session_id {
            self.clear_live_preview();
            return false;
        }
        let Some(reader) = self.live_preview.reader.as_mut() else {
            return false;
        };
        if reader.is_terminal() {
            // Durable terminal delivery still owns committing/restoring timeline entries.
            self.live_preview.reader = None;
            self.live_preview.pending.clear();
            self.live_preview.execution_pending.clear();
            return false;
        }
        let updates = match reader.poll_updates() {
            Ok(updates) => updates,
            Err(error) => {
                self.last_notice = Some(format!("live preview unavailable: {error}"));
                self.clear_live_preview();
                return true;
            }
        };
        self.accept_live_updates(updates);
        self.apply_ready_live_updates()
    }

    fn accept_live_updates(&mut self, updates: Vec<LiveRunUpdate>) {
        for update in updates {
            if update.validate().is_err()
                || update.session_id != self.session_id
                || update.session_id != self.live_preview.session_id
                || update.run_id != self.live_preview.run_id
            {
                continue;
            }
            if update.kind == LiveRunUpdateKind::ToolProgress {
                let Some(progress) = &update.tool_progress else {
                    continue;
                };
                if self
                    .live_preview
                    .execution_revisions
                    .get(&update.slot_id)
                    .is_some_and(|revision| *revision >= update.live_revision)
                {
                    continue;
                }
                if !self
                    .live_preview
                    .execution_revisions
                    .contains_key(&update.slot_id)
                    && self.live_preview.execution_revisions.len() >= 4
                    && let Some(oldest) = self
                        .live_preview
                        .execution_revisions
                        .iter()
                        .min_by_key(|(_, revision)| **revision)
                        .map(|(id, _)| id.clone())
                {
                    self.live_preview.execution_revisions.remove(&oldest);
                    self.live_preview.execution_calls.remove(&oldest);
                    self.live_preview.execution_pending.remove(&oldest);
                }
                self.live_preview
                    .execution_calls
                    .insert(update.slot_id.clone(), progress.call_id.clone());
                self.live_preview
                    .execution_revisions
                    .insert(update.slot_id.clone(), update.live_revision);
                self.live_preview
                    .execution_pending
                    .insert(update.slot_id.clone(), update);
                continue;
            }
            if self.live_preview.attempt_id != update.attempt_id {
                // Producer revisions increase across physical attempts. An old attempt can
                // never replace the current one even if its durable base becomes ready later.
                if update.live_revision <= self.live_preview.latest_revision {
                    continue;
                }
                self.live_preview.pending.clear();
                self.live_preview.slots.clear();
                self.live_preview.revisions.clear();
                self.live_preview.retired.clear();
                self.live_preview.retired_arguments.clear();
                self.discard_provisional_provider_output();
                self.live_preview.attempt_id = update.attempt_id.clone();
            }
            if self.live_preview.retired.contains(&update.slot_id)
                || (update.kind == LiveRunUpdateKind::ToolCallArguments
                    && self
                        .live_preview
                        .retired_arguments
                        .contains(&update.slot_id))
                || self
                    .live_preview
                    .revisions
                    .get(&update.slot_id)
                    .is_some_and(|revision| *revision >= update.live_revision)
            {
                continue;
            }
            if !self.live_preview.revisions.contains_key(&update.slot_id)
                && self.live_preview.revisions.len() >= 4
            {
                continue;
            }
            self.live_preview.latest_revision =
                self.live_preview.latest_revision.max(update.live_revision);
            self.live_preview
                .revisions
                .insert(update.slot_id.clone(), update.live_revision);
            self.live_preview
                .pending
                .insert(update.slot_id.clone(), update);
        }
    }

    fn apply_ready_live_updates(&mut self) -> bool {
        let ready = self
            .live_preview
            .pending
            .iter()
            .filter(|(_, update)| {
                update.base_durable_sequence <= self.live_preview.durable_sequence
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut changed = self.apply_ready_execution_updates();
        for key in ready {
            let Some(update) = self.live_preview.pending.remove(&key) else {
                continue;
            };
            let preview = if update.truncated {
                format!(
                    "{}\n[Live preview truncated; complete output follows.]",
                    update.preview.as_str()
                )
            } else {
                update.preview.as_str().to_owned()
            };
            let role = match update.kind {
                LiveRunUpdateKind::Text => {
                    self.runtime.run_phase = RunPhase::Streaming;
                    self.push_phase_marker("streaming".to_owned());
                    TimelineRole::Assistant
                }
                LiveRunUpdateKind::Reasoning => {
                    self.runtime.run_phase = RunPhase::Thinking;
                    self.push_phase_marker(format!("thinking|{}", self.runtime.model_name));
                    TimelineRole::Thinking
                }
                LiveRunUpdateKind::ToolCallArguments => {
                    if !matches!(self.runtime.run_phase, RunPhase::Tool(_)) {
                        self.runtime.run_phase = RunPhase::Tool("tool".to_owned());
                    }
                    changed = true;
                    continue;
                }
                LiveRunUpdateKind::ToolProgress => continue,
            };
            let prior = self.live_preview.slots.get(&update.slot_id).copied();
            let index = if let Some(index) = prior.filter(|index| {
                self.timeline
                    .get(*index)
                    .is_some_and(|entry| entry.role == role)
            }) {
                self.timeline[index].text = preview;
                self.rerender_timeline_entry_deferred(index);
                index
            } else {
                self.push_timeline(role, preview);
                let Some(index) = self.timeline.len().checked_sub(1) else {
                    continue;
                };
                index
            };
            if role == TimelineRole::Assistant {
                self.timeline_state.streaming_assistant_index = Some(index);
            } else {
                self.timeline_state.streaming_reasoning_index = Some(index);
            }
            self.timeline_state
                .provisional_provider_output_indices
                .insert(index);
            self.live_preview.slots.insert(update.slot_id, index);
            changed = true;
        }
        changed
    }

    fn apply_ready_execution_updates(&mut self) -> bool {
        let ready = self
            .live_preview
            .execution_pending
            .iter()
            .filter(|(_, update)| {
                update.base_durable_sequence <= self.live_preview.durable_sequence
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        let mut changed = false;
        for key in ready {
            let Some(update) = self.live_preview.execution_pending.remove(&key) else {
                continue;
            };
            let Some(progress) = update.tool_progress else {
                continue;
            };
            // The matching completed call must already be delivered. A final result removes
            // it, so delayed snapshots cannot resurrect a finished execution card.
            if !self.safe_tool_calls.contains_key(&progress.call_id) {
                continue;
            }
            let preview = if update.truncated {
                format!(
                    "{}\n[Live preview truncated; complete output follows.]",
                    update.preview.as_str()
                )
            } else {
                update.preview.as_str().to_owned()
            };
            match sigil_kernel::ToolExecutionId::new(progress.execution_id) {
                Ok(execution_id) => {
                    let event = sigil_kernel::ToolProgressEvent {
                        execution_id,
                        call_id: progress.call_id,
                        tool_name: progress.tool_name,
                        sequence: update.live_revision,
                        status: progress.status,
                        message: (!progress.preview_is_output).then(|| preview.clone()),
                        output_preview: progress.preview_is_output.then_some(preview),
                        output_log_ref: None,
                        total_bytes: progress.total_bytes,
                        updated_at_ms: progress.updated_at_ms,
                        details: serde_json::json!({ "started_at_ms": progress.started_at_ms }),
                    };
                    if let Err(error) = sigil_kernel::EventHandler::handle(
                        self,
                        sigil_kernel::RunEvent::ToolProgress(event),
                    ) {
                        self.last_notice = Some(format!("tool preview unavailable: {error}"));
                    }
                }
                Err(error) => {
                    self.last_notice = Some(format!("tool preview identity invalid: {error}"))
                }
            }
            changed = true;
        }
        changed
    }

    pub(super) fn remap_live_preview_after_entry_removal(&mut self, removed: &[usize]) {
        self.live_preview.slots.retain(|_, index| {
            if removed.contains(index) {
                return false;
            }
            *index =
                index.saturating_sub(removed.iter().filter(|removed| **removed < *index).count());
            true
        });
    }

    pub(super) fn retire_live_tool_arguments(&mut self, call_id: &str) {
        self.live_preview.pending.remove(call_id);
        if self.live_preview.revisions.contains_key(call_id) {
            self.live_preview
                .retired_arguments
                .insert(call_id.to_owned());
        }
    }

    pub(super) fn retire_live_tool_slot(&mut self, call_id: &str) {
        let executions = self
            .live_preview
            .execution_calls
            .iter()
            .filter(|(_, call)| call.as_str() == call_id)
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in executions {
            self.live_preview.execution_calls.remove(&id);
            self.live_preview.execution_revisions.remove(&id);
            self.live_preview.execution_pending.remove(&id);
        }
        self.live_preview.pending.remove(call_id);
        self.live_preview.slots.remove(call_id);
        if self.live_preview.revisions.contains_key(call_id) {
            self.live_preview.retired.insert(call_id.to_owned());
        }
    }

    pub(super) fn replace_live_assistant_message(&mut self, message: &sigil_kernel::ModelMessage) {
        let (slot, role) =
            if message.assistant_kind == Some(sigil_kernel::AssistantMessageKind::ReasoningTrace) {
                ("assistant-reasoning", TimelineRole::Thinking)
            } else {
                ("assistant-text", TimelineRole::Assistant)
            };
        self.live_preview.pending.remove(slot);
        if self.live_preview.revisions.contains_key(slot) {
            self.live_preview.retired.insert(slot.to_owned());
        }
        if let Some(index) = self.live_preview.slots.remove(slot)
            && let Some(content) = &message.content
            && let Some(entry) = self.timeline.get_mut(index)
            && entry.role == role
        {
            entry.text = content.clone();
            self.rerender_timeline_entry_deferred(index);
        }
    }
}

#[cfg(test)]
#[path = "tests/live_preview_flow_tests.rs"]
mod tests;
