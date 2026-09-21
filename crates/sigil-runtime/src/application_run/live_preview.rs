use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow, ensure};
use sigil_application::{LiveRunUpdate, LiveRunUpdateKind, MAX_SAFE_TEXT_BYTES, SafeText};
use sigil_kernel::PublicRunEventKind;

pub const MAX_LIVE_PREVIEW_SLOTS: usize = 4;
pub const LIVE_PREVIEW_FRAME_INTERVAL: Duration = Duration::from_millis(32);

/// Read-only, process-local view of one execution owner's bounded preview slots.
/// Readers retain no delta queue and cannot advance the durable delivery cursor.
#[derive(Debug, Clone)]
pub struct RuntimeLivePreviewSource {
    session_id: String,
    run_id: String,
    state: Arc<Mutex<PreviewState>>,
}

#[derive(Debug, Default)]
struct PreviewState {
    attempt_id: Option<String>,
    revision: u64,
    slots: BTreeMap<String, PreviewSlot>,
    executions: BTreeMap<String, ExecutionPreview>,
    execution_slots: BTreeMap<String, PreviewSlot>,
    admitted_slots: BTreeSet<String>,
    retired_tool_slots: BTreeSet<String>,
    retired_tool_arguments: BTreeSet<String>,
    text_retired: bool,
    reasoning_retired: bool,
    attempt_closed: bool,
    terminal: bool,
}

#[derive(Debug)]
struct ExecutionPreview {
    tool_name: String,
    execution_id: Option<String>,
    started_at_ms: Option<u64>,
}

#[derive(Debug)]
struct PreviewSlot {
    kind: LiveRunUpdateKind,
    text: String,
    revision: u64,
    base: u64,
    truncated: bool,
    tool_progress: Option<sigil_application::LiveToolProgress>,
}

impl RuntimeLivePreviewSource {
    pub(super) fn new(session_id: &str, run_id: &str, terminal: bool) -> Self {
        Self {
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            state: Arc::new(Mutex::new(PreviewState {
                terminal,
                ..PreviewState::default()
            })),
        }
    }

    /// Returns the immutable durable session binding of this preview source.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the actual run selected by the execution owner.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether the foreground owner has committed its terminal and retired every preview.
    pub fn is_terminal(&self) -> bool {
        self.state
            .lock()
            .map(|state| state.terminal)
            .unwrap_or(true)
    }

    /// Creates an independent, paced reader. The first poll includes the current snapshots,
    /// which permits reconnect after the client has restored its durable projection.
    pub fn reader(&self) -> RuntimeLivePreviewReader {
        RuntimeLivePreviewReader {
            source: self.clone(),
            last_poll: None,
            revisions: BTreeMap::new(),
        }
    }

    pub(super) fn begin_attempt(&self, attempt_id: &str) -> Result<()> {
        ensure!(
            !attempt_id.is_empty() && attempt_id.len() <= 256,
            "invalid live provider attempt identity"
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("live preview source is unavailable"))?;
        ensure!(!state.terminal, "live preview source is terminal");
        if state.attempt_id.as_deref() != Some(attempt_id) {
            state.slots.clear();
            state.admitted_slots.clear();
            state.retired_tool_slots.clear();
            state.retired_tool_arguments.clear();
            state.text_retired = false;
            state.reasoning_retired = false;
            state.attempt_closed = false;
            state.attempt_id = Some(attempt_id.to_owned());
        }
        Ok(())
    }

    pub(super) fn apply_delta(&self, event: &PublicRunEventKind, base: u64) -> Result<()> {
        let (kind, slot_id, text) = match event {
            PublicRunEventKind::TextDelta { text } => {
                (LiveRunUpdateKind::Text, "assistant-text", text.as_str())
            }
            PublicRunEventKind::ReasoningDelta { text } => (
                LiveRunUpdateKind::Reasoning,
                "assistant-reasoning",
                text.as_str(),
            ),
            PublicRunEventKind::ToolCallArgsDelta { id, delta } => (
                LiveRunUpdateKind::ToolCallArguments,
                id.as_str(),
                delta.as_str(),
            ),
            PublicRunEventKind::ToolProgress { progress } => {
                return self.apply_tool_progress(progress, base);
            }
            _ => return Ok(()),
        };
        if text.is_empty() {
            return Ok(());
        }
        ensure!(
            !slot_id.is_empty() && slot_id.len() <= 256 && !slot_id.chars().any(char::is_control),
            "invalid live semantic slot identity"
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("live preview source is unavailable"))?;
        ensure!(!state.terminal, "live preview source is terminal");
        // Private handlers that do not forward the admitted provider attempt may still record
        // durable events; they must not manufacture a live identity for their parent's surface.
        if state.attempt_id.is_none() || state.attempt_closed {
            return Ok(());
        }
        if (kind == LiveRunUpdateKind::Text && state.text_retired)
            || (kind == LiveRunUpdateKind::Reasoning && state.reasoning_retired)
            || (kind == LiveRunUpdateKind::ToolCallArguments
                && state.retired_tool_arguments.contains(slot_id))
        {
            return Ok(());
        }
        if state.retired_tool_slots.contains(slot_id) {
            return Ok(());
        }
        if state
            .slots
            .get(slot_id)
            .is_some_and(|slot| slot.kind != kind)
        {
            // Provider tool-call IDs are opaque: a collision must not replace another
            // semantic channel's content.
            return Ok(());
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("live preview revision exhausted"))?;
        let revision = state.revision;
        if !state.admitted_slots.contains(slot_id)
            && state.admitted_slots.len() == MAX_LIVE_PREVIEW_SLOTS
        {
            // An undisplayed slot is not admitted midway through a stream: doing so would
            // mislabel a suffix as a complete replacement snapshot.
            return Ok(());
        }
        state.admitted_slots.insert(slot_id.to_owned());
        let slot = state
            .slots
            .entry(slot_id.to_owned())
            .or_insert_with(|| PreviewSlot {
                kind,
                text: String::with_capacity(MAX_SAFE_TEXT_BYTES),
                revision,
                base,
                truncated: false,
                tool_progress: None,
            });
        slot.kind = kind;
        let budget = MAX_SAFE_TEXT_BYTES.saturating_sub(slot.text.len());
        let mut end = text.len().min(budget);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        slot.text.push_str(&text[..end]);
        slot.truncated |= end < text.len();
        slot.revision = revision;
        slot.base = base;
        Ok(())
    }

    fn apply_tool_progress(
        &self,
        progress: &sigil_kernel::ToolProgressEvent,
        base: u64,
    ) -> Result<()> {
        for value in [
            progress.execution_id.as_str(),
            progress.call_id.as_str(),
            progress.tool_name.as_str(),
            progress.status.as_str(),
        ] {
            ensure!(
                !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control),
                "invalid live tool progress metadata"
            );
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("live preview source is unavailable"))?;
        ensure!(!state.terminal, "live preview source is terminal");
        // The completed, safely projected call admits progress independently from any provider
        // attempt. Removing this binding on the result also rejects late producer snapshots.
        let Some(binding) = state.executions.get(&progress.call_id) else {
            return Ok(());
        };
        if binding.tool_name != progress.tool_name
            || binding
                .execution_id
                .as_deref()
                .is_some_and(|id| id != progress.execution_id.as_str())
            || state.executions.iter().any(|(call, binding)| {
                call != &progress.call_id
                    && binding.execution_id.as_deref() == Some(progress.execution_id.as_str())
            })
        {
            return Ok(());
        }
        let started_at_ms = if let Some(binding) = state.executions.get_mut(&progress.call_id) {
            binding.execution_id = Some(progress.execution_id.as_str().to_owned());
            binding.started_at_ms = binding.started_at_ms.or_else(|| {
                progress
                    .details
                    .get("started_at_ms")
                    .and_then(serde_json::Value::as_u64)
            });
            binding.started_at_ms
        } else {
            None
        };
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("live preview revision exhausted"))?;
        let revision = state.revision;
        let execution_id = progress.execution_id.as_str();
        if !state.execution_slots.contains_key(execution_id)
            && state.execution_slots.len() >= MAX_LIVE_PREVIEW_SLOTS
            && let Some(oldest) = state
                .execution_slots
                .iter()
                .min_by_key(|(_, slot)| slot.revision)
                .map(|(id, _)| id.clone())
        {
            state.execution_slots.remove(&oldest);
        }
        let output = progress
            .output_preview
            .as_deref()
            .filter(|text| !text.is_empty());
        let text = output
            .or(progress.message.as_deref().filter(|text| !text.is_empty()))
            .unwrap_or(&progress.status);
        let mut end = text.len().min(MAX_SAFE_TEXT_BYTES);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        state.execution_slots.insert(
            execution_id.to_owned(),
            PreviewSlot {
                kind: LiveRunUpdateKind::ToolProgress,
                text: text[..end].to_owned(),
                revision,
                base,
                truncated: end < text.len(),
                tool_progress: Some(sigil_application::LiveToolProgress {
                    execution_id: execution_id.to_owned(),
                    call_id: progress.call_id.clone(),
                    tool_name: progress.tool_name.clone(),
                    status: progress.status.clone(),
                    preview_is_output: output.is_some(),
                    started_at_ms,
                    total_bytes: progress.total_bytes,
                    updated_at_ms: progress.updated_at_ms,
                }),
            },
        );
        Ok(())
    }

    pub(super) fn apply_committed(&self, event: &PublicRunEventKind, terminal: bool) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if terminal {
            state.slots.clear();
            state.executions.clear();
            state.execution_slots.clear();
            state.terminal = true;
            return;
        }
        match event {
            PublicRunEventKind::AssistantMessage { message } => {
                let replaced = if message.assistant_kind
                    == Some(sigil_kernel::AssistantMessageKind::ReasoningTrace)
                {
                    LiveRunUpdateKind::Reasoning
                } else {
                    LiveRunUpdateKind::Text
                };
                state.slots.retain(|_, slot| slot.kind != replaced);
                match replaced {
                    LiveRunUpdateKind::Text => state.text_retired = true,
                    LiveRunUpdateKind::Reasoning => state.reasoning_retired = true,
                    _ => {}
                }
            }
            PublicRunEventKind::ToolCallCompleted { call } => {
                state
                    .executions
                    .entry(call.id.clone())
                    .or_insert_with(|| ExecutionPreview {
                        tool_name: call.name.clone(),
                        execution_id: None,
                        started_at_ms: None,
                    });
                state.slots.remove(&call.id);
                if state.admitted_slots.contains(&call.id) {
                    state.retired_tool_arguments.insert(call.id.clone());
                }
            }
            PublicRunEventKind::ToolResult { result } => {
                if let Some(binding) = state.executions.remove(&result.call_id)
                    && let Some(execution_id) = binding.execution_id
                {
                    state.execution_slots.remove(&execution_id);
                }
                state.slots.remove(&result.call_id);
                if state.admitted_slots.contains(&result.call_id) {
                    state.retired_tool_slots.insert(result.call_id.clone());
                }
            }
            PublicRunEventKind::ProviderTurnPartialOutputDiscarded { .. }
            | PublicRunEventKind::RunAwaitingUserInput { .. } => {
                state.slots.clear();
                state.attempt_closed = true;
            }
            _ => {}
        }
    }
}

/// Per-attachment cursor for preview revisions, independent from durable event delivery.
#[derive(Debug)]
pub struct RuntimeLivePreviewReader {
    source: RuntimeLivePreviewSource,
    last_poll: Option<Instant>,
    revisions: BTreeMap<(bool, String), (Option<String>, u64)>,
}

impl RuntimeLivePreviewReader {
    /// Whether the source has retired its preview slots after a durable terminal.
    pub fn is_terminal(&self) -> bool {
        self.source.is_terminal()
    }

    /// Takes changed replacement snapshots at most once per 32 ms. There are never more than
    /// four provider previews and four latest execution snapshots, each bounded before allocation
    /// by the producer to 64 KiB of UTF-8. Execution snapshots replace, never append, output.
    pub fn poll_updates(&mut self) -> Result<Vec<LiveRunUpdate>> {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Result<Vec<LiveRunUpdate>> {
        if self
            .last_poll
            .is_some_and(|last| now.saturating_duration_since(last) < LIVE_PREVIEW_FRAME_INTERVAL)
        {
            return Ok(Vec::new());
        }
        self.last_poll = Some(now);
        let state = self
            .source
            .state
            .lock()
            .map_err(|_| anyhow!("live preview source is unavailable"))?;
        self.revisions.retain(|(execution, id), _| {
            if *execution {
                state.execution_slots.contains_key(id)
            } else {
                state.slots.contains_key(id)
            }
        });
        let mut updates = Vec::with_capacity(state.slots.len() + state.execution_slots.len());
        for (execution, slot_id, slot) in state
            .slots
            .iter()
            .map(|(id, slot)| (false, id, slot))
            .chain(
                state
                    .execution_slots
                    .iter()
                    .map(|(id, slot)| (true, id, slot)),
            )
        {
            let attempt_id = if execution {
                None
            } else {
                state.attempt_id.clone()
            };
            let key = (execution, slot_id.clone());
            if self.revisions.get(&key).is_some_and(|(attempt, revision)| {
                attempt == &attempt_id && *revision >= slot.revision
            }) {
                continue;
            }
            if slot.text.is_empty() {
                continue;
            }
            let update = LiveRunUpdate {
                schema_version: sigil_application::APPLICATION_CONTRACT_SCHEMA_VERSION,
                session_id: self.source.session_id.clone(),
                run_id: self.source.run_id.clone(),
                attempt_id: attempt_id.clone(),
                slot_id: slot_id.clone(),
                live_revision: slot.revision,
                base_durable_sequence: slot.base,
                kind: slot.kind,
                preview: SafeText::new(slot.text.clone())?,
                tool_progress: slot.tool_progress.clone(),
                truncated: slot.truncated,
            };
            update.validate()?;
            self.revisions.insert(key, (attempt_id, slot.revision));
            updates.push(update);
        }
        Ok(updates)
    }
}

#[cfg(test)]
#[path = "../tests/live_preview_tests.rs"]
mod tests;
