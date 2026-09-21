use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use serde::Serialize;
use sigil_desktop::{
    DesktopApprovalLifecycleState, DesktopHttpClient, DesktopRunSnapshot, DesktopRunStatus,
    DesktopTerminalTaskStatus, DesktopTimelineEvent, DesktopTimelineEventKind,
    DesktopTimelineTerminalTask,
};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

pub(crate) const DESKTOP_RUN_EVENT_NAME: &str = "sigil-run-event";
pub(crate) const DESKTOP_RUN_STREAM_STATUS_NAME: &str = "sigil-run-stream-status";
pub(crate) const DESKTOP_RUN_APPROVAL_SNAPSHOT_NAME: &str = "sigil-run-approval-snapshot";

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const MIN_HEALTHY_STREAM_LIFETIME: Duration = Duration::from_secs(10);
const MAX_RECONNECT_ATTEMPTS: u8 = 8;
const MAX_ATTACHMENT_EVENTS: usize = 512;
const MAX_ATTACHMENT_TEXT_BYTES: usize = 2 * 1024 * 1024;
const MAX_PENDING_APPROVALS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DesktopRunStreamState {
    Connecting,
    Live,
    Reconnecting,
    Terminal,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunStreamStatus {
    pub(crate) workspace_id: String,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) state: DesktopRunStreamState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<&'static str>,
}

struct OwnedRunStream {
    workspace_id: String,
    renderer_session_id: String,
    durable_session_id: String,
    task: Option<JoinHandle<()>>,
    projection: RunProjection,
}

struct RunProjection {
    event_cursor: RunEventCursor,
    events: VecDeque<DesktopTimelineEvent>,
    event_text_bytes: usize,
    pending_approvals: BTreeMap<String, DesktopTimelineEvent>,
    terminal_tasks: BTreeMap<String, DesktopTimelineEvent>,
    has_gap: bool,
    last_sequence: u64,
    last_replay_id: Option<String>,
    last_registry_revision: u64,
    stream_state: DesktopRunStreamState,
    stream_message: Option<&'static str>,
    run_status: DesktopRunStatus,
    task_pause_observed: bool,
}

#[derive(Default)]
struct ExecutionPreviewBinding {
    tool_name: String,
    admitted_sequence: u64,
    execution_id: Option<String>,
    revision: u64,
}

/// The public cursor advances only on durable payloads. Preview slots have independent
/// revisions and cannot reopen a message replaced by a later committed publication.
#[derive(Default)]
struct RunEventCursor {
    durable_sequence: u64,
    retired_preview_sequence: u64,
    terminal: bool,
    attempt: Option<String>,
    latest_live_revision: u64,
    live_revisions: BTreeMap<String, u64>,
    execution_calls: BTreeMap<String, ExecutionPreviewBinding>,
    terminal_execution_ids: BTreeMap<String, u64>,
    retired_tool_slots: BTreeSet<String>,
    retired_tool_arguments: BTreeSet<String>,
    text_retired: bool,
    reasoning_retired: bool,
    attempt_closed: bool,
}

impl RunEventCursor {
    fn accept(&mut self, event: &DesktopTimelineEvent) -> bool {
        if let Some(preview) = &event.live_preview {
            let (Ok(revision), Ok(base)) = (
                preview.revision.parse::<u64>(),
                preview.base_sequence.parse::<u64>(),
            ) else {
                return false;
            };
            if self.terminal
                || revision == 0
                || base > self.durable_sequence
                || event.replayable
                || event.replay_id.is_some()
                || event.sequence != base
                || event.run_sequence != preview.base_sequence
            {
                return false;
            }
            if event.kind == DesktopTimelineEventKind::ToolProgress {
                return self.accept_execution_preview(event, revision);
            }
            let Some(attempt) = preview
                .attempt_id
                .as_deref()
                .filter(|id| valid_preview_identity(id))
            else {
                return false;
            };
            if event.execution_id.is_some()
                || !valid_preview_identity(&preview.slot_id)
                || base < self.retired_preview_sequence
            {
                return false;
            }
            if self.attempt.as_deref() != Some(attempt) {
                if revision <= self.latest_live_revision {
                    return false;
                }
                self.attempt = Some(attempt.to_owned());
                self.live_revisions.clear();
                self.retired_tool_slots.clear();
                self.retired_tool_arguments.clear();
                self.text_retired = false;
                self.reasoning_retired = false;
                self.attempt_closed = false;
            }
            if self.attempt_closed
                || self.retired_tool_slots.contains(&preview.slot_id)
                || (event.kind == DesktopTimelineEventKind::AssistantDelta && self.text_retired)
                || (event.kind == DesktopTimelineEventKind::ReasoningDelta
                    && self.reasoning_retired)
                || (event.kind == DesktopTimelineEventKind::ToolCallArgsDelta
                    && self.retired_tool_arguments.contains(&preview.slot_id))
            {
                return false;
            }
            if self
                .live_revisions
                .get(&preview.slot_id)
                .is_some_and(|previous| *previous >= revision)
            {
                return false;
            }
            if !self.live_revisions.contains_key(&preview.slot_id)
                && self.live_revisions.len() == 4
                && let Some(oldest) = self
                    .live_revisions
                    .iter()
                    .min_by_key(|(_, revision)| **revision)
                    .map(|(slot, _)| slot.clone())
            {
                self.live_revisions.remove(&oldest);
            }
            self.live_revisions
                .insert(preview.slot_id.clone(), revision);
            self.latest_live_revision = self.latest_live_revision.max(revision);
            return true;
        }
        if event.sequence <= self.durable_sequence {
            return false;
        }
        self.retire_terminal_execution(event, true);
        if event.replayable {
            self.durable_sequence = event.sequence;
            if event.kind == DesktopTimelineEventKind::RunStarted {
                *self = Self {
                    durable_sequence: event.sequence,
                    retired_preview_sequence: event.sequence,
                    ..Self::default()
                };
            }
            if event.kind == DesktopTimelineEventKind::AssistantMessage {
                if event.assistant_kind.as_deref() == Some("reasoning_trace") {
                    self.reasoning_retired = true;
                } else {
                    self.text_retired = true;
                }
            }
            if event.kind == DesktopTimelineEventKind::ToolCompleted
                && let Some(slot) = event
                    .item_id
                    .as_ref()
                    .filter(|id| valid_preview_identity(id))
            {
                if self.retired_tool_arguments.len() < 4 {
                    self.retired_tool_arguments.insert(slot.clone());
                }
                if self.execution_calls.len() < MAX_ATTACHMENT_EVENTS
                    && let Some(tool_name) = event
                        .tool_name
                        .as_ref()
                        .filter(|name| valid_preview_identity(name))
                {
                    self.execution_calls.entry(slot.clone()).or_insert_with(|| {
                        ExecutionPreviewBinding {
                            tool_name: tool_name.clone(),
                            admitted_sequence: event.sequence,
                            ..ExecutionPreviewBinding::default()
                        }
                    });
                }
            }
            if event.kind == DesktopTimelineEventKind::ToolResult
                && let Some(call) = &event.item_id
            {
                self.execution_calls.remove(call);
                if let Some(execution) = &event.execution_id {
                    self.terminal_execution_ids.remove(execution);
                }
                self.prune_terminal_previews();
            }
            if event.kind == DesktopTimelineEventKind::ProviderTurnPartialOutputDiscarded
                || (event.kind == DesktopTimelineEventKind::UserInputChanged
                    && event.status.as_deref() == Some("requested"))
            {
                self.attempt_closed = true;
                self.retired_preview_sequence = event.sequence;
                self.live_revisions.clear();
            }
            if event.kind == DesktopTimelineEventKind::ToolResult
                && let Some(slot) = &event.item_id
                && self.retired_tool_slots.len() < 4
            {
                self.retired_tool_slots.insert(slot.clone());
            }
            if matches!(
                event.kind,
                DesktopTimelineEventKind::AssistantMessage
                    | DesktopTimelineEventKind::ProviderTurnPartialOutputDiscarded
                    | DesktopTimelineEventKind::ToolCompleted
                    | DesktopTimelineEventKind::ToolResult
            ) {
                self.retired_preview_sequence = event.sequence;
                self.live_revisions.clear();
            }
            if matches!(
                event.kind,
                DesktopTimelineEventKind::RunFinished
                    | DesktopTimelineEventKind::RunFailed
                    | DesktopTimelineEventKind::RunBlocked
                    | DesktopTimelineEventKind::RunPaused
                    | DesktopTimelineEventKind::RunInterrupted
                    | DesktopTimelineEventKind::RunCancelled
            ) {
                self.terminal = true;
                self.live_revisions.clear();
                self.execution_calls.clear();
            }
        }
        true
    }

    fn prune_terminal_previews(&mut self) {
        self.terminal_execution_ids.retain(|_, sequence| {
            self.execution_calls.values().any(|binding| {
                binding.execution_id.is_none() && binding.admitted_sequence <= *sequence
            })
        });
    }

    fn retire_terminal_execution(&mut self, event: &DesktopTimelineEvent, track_unbound: bool) {
        if let Some(execution) = terminal_execution_id(event) {
            self.execution_calls
                .retain(|_, binding| binding.execution_id.as_deref() != Some(execution));
            self.prune_terminal_previews();
            if track_unbound
                && self.execution_calls.values().any(|binding| {
                    binding.execution_id.is_none() && binding.admitted_sequence <= event.sequence
                })
            {
                if self.terminal_execution_ids.len() == MAX_ATTACHMENT_EVENTS
                    && !self.terminal_execution_ids.contains_key(execution)
                {
                    // Only unresolved admissions older than this overflow lose ephemeral progress;
                    // later commands remain admissible and durable command cards remain intact.
                    self.execution_calls.retain(|_, binding| {
                        binding.execution_id.is_some() || binding.admitted_sequence > event.sequence
                    });
                    self.prune_terminal_previews();
                } else {
                    self.terminal_execution_ids
                        .insert(execution.to_owned(), event.sequence);
                }
            }
        }
    }

    fn accept_execution_preview(&mut self, event: &DesktopTimelineEvent, revision: u64) -> bool {
        let Some(preview) = &event.live_preview else {
            return false;
        };
        let (Some(execution), Some(call)) =
            (event.execution_id.as_deref(), event.item_id.as_deref())
        else {
            return false;
        };
        if self.terminal_execution_ids.contains_key(execution)
            || preview.attempt_id.is_some()
            || !valid_preview_identity(execution)
            || !valid_preview_identity(call)
            || preview.slot_id != execution
        {
            return false;
        }
        if self.execution_calls.iter().any(|(other_call, binding)| {
            other_call != call && binding.execution_id.as_deref() == Some(execution)
        }) {
            return false;
        }
        let Some(binding) = self.execution_calls.get_mut(call) else {
            return false;
        };
        if event.tool_name.as_deref() != Some(binding.tool_name.as_str())
            || event.sequence < binding.admitted_sequence
            || binding.revision >= revision
            || binding
                .execution_id
                .as_deref()
                .is_some_and(|id| id != execution)
        {
            return false;
        }
        binding.execution_id = Some(execution.to_owned());
        binding.revision = revision;
        true
    }
}

fn retain_preview_after_event(old: &DesktopTimelineEvent, event: &DesktopTimelineEvent) -> bool {
    if old.live_preview.is_none() {
        return true;
    }
    if matches!(
        event.kind,
        DesktopTimelineEventKind::RunStarted
            | DesktopTimelineEventKind::RunFinished
            | DesktopTimelineEventKind::RunFailed
            | DesktopTimelineEventKind::RunBlocked
            | DesktopTimelineEventKind::RunPaused
            | DesktopTimelineEventKind::RunInterrupted
            | DesktopTimelineEventKind::RunCancelled
    ) {
        return false;
    }
    if is_execution_preview(old) {
        return (event.kind != DesktopTimelineEventKind::ToolResult
            || old.item_id != event.item_id)
            && terminal_execution_id(event)
                .is_none_or(|execution| old.execution_id.as_deref() != Some(execution));
    }
    match event.kind {
        DesktopTimelineEventKind::ProviderTurnPartialOutputDiscarded => false,
        DesktopTimelineEventKind::UserInputChanged
            if event.status.as_deref() == Some("requested") =>
        {
            false
        }
        DesktopTimelineEventKind::ToolCompleted | DesktopTimelineEventKind::ToolResult => {
            old.item_id != event.item_id
        }
        DesktopTimelineEventKind::AssistantMessage => {
            old.kind
                != if event.assistant_kind.as_deref() == Some("reasoning_trace") {
                    DesktopTimelineEventKind::ReasoningDelta
                } else {
                    DesktopTimelineEventKind::AssistantDelta
                }
        }
        _ => true,
    }
}

fn terminal_execution_id(event: &DesktopTimelineEvent) -> Option<&str> {
    event
        .terminal_task
        .as_ref()
        .filter(|task| {
            matches!(
                task.status.as_str(),
                "exited" | "failed" | "cancelled" | "interrupted"
            )
        })
        .map(|task| task.task_id.as_str())
}

fn valid_preview_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn is_execution_preview(event: &DesktopTimelineEvent) -> bool {
    event.live_preview.is_some()
        && event.kind == DesktopTimelineEventKind::ToolProgress
        && event.execution_id.is_some()
}

struct RunSnapshotReconciliation {
    approval_snapshot: DesktopRunApprovalSnapshot,
    terminal_events: Vec<DesktopTimelineEvent>,
    settled: bool,
}

pub(crate) struct DesktopRunProjectionSnapshot {
    pub(crate) events: Vec<DesktopTimelineEvent>,
    pub(crate) has_gap: bool,
    pub(crate) stream_state: DesktopRunStreamState,
    pub(crate) stream_message: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunApprovalSnapshot {
    pub(crate) workspace_id: String,
    pub(crate) session_id: String,
    pub(crate) run_id: String,
    pub(crate) registry_revision: u64,
    pub(crate) pending_approvals: Vec<DesktopTimelineEvent>,
    pub(crate) approval_lifecycles: Vec<DesktopRunApprovalLifecycleSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRunApprovalLifecycleSnapshot {
    pub(crate) event: DesktopTimelineEvent,
    pub(crate) state: DesktopApprovalLifecycleState,
}

/// Owns every background SSE follower so workspace close and app exit cannot detach work.
#[derive(Clone, Default)]
pub(crate) struct DesktopRunStreamOwner {
    streams: Arc<Mutex<BTreeMap<String, OwnedRunStream>>>,
}

impl DesktopRunStreamOwner {
    pub(crate) async fn start(
        &self,
        app: AppHandle,
        client: DesktopHttpClient,
        workspace_id: String,
        renderer_session_id: String,
        durable_session_id: String,
        owner_revision: String,
        run: DesktopRunSnapshot,
    ) {
        let _ = self
            .attach_inner(
                app,
                client,
                workspace_id,
                renderer_session_id,
                durable_session_id,
                owner_revision,
                run,
                false,
            )
            .await;
    }

    pub(crate) async fn attach(
        &self,
        app: AppHandle,
        client: DesktopHttpClient,
        workspace_id: String,
        renderer_session_id: String,
        durable_session_id: String,
        owner_revision: String,
        run: DesktopRunSnapshot,
    ) -> DesktopRunProjectionSnapshot {
        if let Some(snapshot) = settled_terminal_reattach_snapshot(&run) {
            return snapshot;
        }
        let initial_gap = run.stream_sequence > 0;
        self.attach_inner(
            app,
            client,
            workspace_id,
            renderer_session_id,
            durable_session_id,
            owner_revision,
            run,
            initial_gap,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn attach_inner(
        &self,
        app: AppHandle,
        client: DesktopHttpClient,
        workspace_id: String,
        renderer_session_id: String,
        durable_session_id: String,
        owner_revision: String,
        run: DesktopRunSnapshot,
        initial_gap: bool,
    ) -> DesktopRunProjectionSnapshot {
        let run_id = run.id.clone();
        let key = stream_key(&workspace_id, &run_id);
        let mut streams = self.streams.lock().await;
        streams.retain(|candidate_key, stream| {
            candidate_key == &key
                || stream.workspace_id != workspace_id
                || !stream.projection.is_settled()
        });
        let stream = streams
            .entry(key.clone())
            .or_insert_with(|| OwnedRunStream {
                workspace_id: workspace_id.clone(),
                renderer_session_id: renderer_session_id.clone(),
                durable_session_id: durable_session_id.clone(),
                task: None,
                projection: RunProjection::new(run.status, initial_gap),
            });
        stream.renderer_session_id.clone_from(&renderer_session_id);
        stream.durable_session_id.clone_from(&durable_session_id);
        stream.projection.run_status = run.status;
        stream
            .projection
            .reconcile_run_snapshot(&run, &workspace_id, &renderer_session_id);

        let follower_finished = stream
            .task
            .as_ref()
            .is_none_or(|task| task.inner().is_finished());
        if stream.projection.is_settled() {
            stream.projection.stream_state = DesktopRunStreamState::Terminal;
        } else if follower_finished {
            if let Some(previous) = stream.task.take() {
                previous.abort();
            }
            stream.projection.stream_state = DesktopRunStreamState::Connecting;
            stream.projection.stream_message = None;
            let initial_cursor = stream.projection.last_replay_id.clone();
            let owner = self.clone();
            stream.task = Some(tauri::async_runtime::spawn(follow_run(
                owner,
                app,
                client,
                workspace_id,
                renderer_session_id,
                durable_session_id,
                owner_revision,
                run,
                initial_cursor,
            )));
        }
        stream.projection.snapshot()
    }

    pub(crate) async fn stop_workspace(&self, workspace_id: &str) {
        let mut streams = self.streams.lock().await;
        let keys = streams
            .iter()
            .filter(|(_, stream)| stream.workspace_id == workspace_id)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(stream) = streams.remove(&key)
                && let Some(task) = stream.task
            {
                task.abort();
            }
        }
    }

    pub(crate) async fn stop_all(&self) {
        let streams = std::mem::take(&mut *self.streams.lock().await);
        for stream in streams.into_values() {
            if let Some(task) = stream.task {
                task.abort();
            }
        }
    }

    async fn record_status(
        &self,
        workspace_id: &str,
        run_id: &str,
        state: DesktopRunStreamState,
        message: Option<&'static str>,
    ) {
        let key = stream_key(workspace_id, run_id);
        let mut streams = self.streams.lock().await;
        let Some(stream) = streams.get_mut(&key) else {
            return;
        };
        stream.projection.stream_state = state;
        stream.projection.stream_message = message;
        if matches!(
            state,
            DesktopRunStreamState::Reconnecting | DesktopRunStreamState::Error
        ) {
            stream.projection.has_gap = true;
        }
        if state == DesktopRunStreamState::Terminal {
            stream.projection.run_status = terminal_status(stream.projection.run_status);
        }
    }

    /// The cursor lives with the retained projection, so a replacement SSE follower preserves
    /// completed-call admission and result retirement instead of inventing a provider attempt.
    async fn record_stream_event(&self, event: DesktopTimelineEvent) -> Option<bool> {
        let key = stream_key(&event.workspace_id, &event.run_id);
        let mut streams = self.streams.lock().await;
        let stream = streams.get_mut(&key)?;
        if !stream.projection.accept_stream_event(&event) {
            return None;
        }
        stream.projection.push(event);
        Some(stream.projection.is_settled())
    }

    async fn reset_event_cursor(&self, workspace_id: &str, run_id: &str) {
        if let Some(stream) = self
            .streams
            .lock()
            .await
            .get_mut(&stream_key(workspace_id, run_id))
        {
            stream.projection.event_cursor = RunEventCursor::default();
            stream.projection.last_sequence = 0;
            stream.projection.last_replay_id = None;
            stream
                .projection
                .events
                .retain(|event| event.live_preview.is_none());
            stream.projection.event_text_bytes =
                stream.projection.events.iter().map(event_text_bytes).sum();
        }
    }

    async fn record_event(&self, event: DesktopTimelineEvent) -> bool {
        let key = stream_key(&event.workspace_id, &event.run_id);
        let mut streams = self.streams.lock().await;
        let Some(stream) = streams.get_mut(&key) else {
            return false;
        };
        stream.projection.push(event);
        stream.projection.is_settled()
    }

    async fn reconcile_run_snapshot(
        &self,
        workspace_id: &str,
        renderer_session_id: &str,
        run: &DesktopRunSnapshot,
    ) -> Option<RunSnapshotReconciliation> {
        let key = stream_key(workspace_id, &run.id);
        let mut streams = self.streams.lock().await;
        let stream = streams.get_mut(&key)?;
        let previous_terminal_generations = stream
            .projection
            .terminal_tasks
            .iter()
            .filter_map(|(task_id, event)| {
                event
                    .terminal_task
                    .as_ref()
                    .map(|task| (task_id.clone(), task.generation))
            })
            .collect::<BTreeMap<_, _>>();
        if !stream
            .projection
            .reconcile_run_snapshot(run, workspace_id, renderer_session_id)
        {
            return None;
        }
        let terminal_events = stream
            .projection
            .terminal_tasks
            .iter()
            .filter(|(task_id, event)| {
                let generation = event
                    .terminal_task
                    .as_ref()
                    .map_or(0, |task| task.generation);
                previous_terminal_generations
                    .get(*task_id)
                    .is_none_or(|previous| *previous < generation)
            })
            .map(|(_, event)| event.clone())
            .collect();
        Some(RunSnapshotReconciliation {
            approval_snapshot: DesktopRunApprovalSnapshot {
                workspace_id: workspace_id.to_owned(),
                session_id: renderer_session_id.to_owned(),
                run_id: run.id.clone(),
                registry_revision: run.stream_sequence,
                pending_approvals: stream
                    .projection
                    .pending_approvals
                    .values()
                    .cloned()
                    .collect(),
                approval_lifecycles: run
                    .approval_lifecycles
                    .clone()
                    .into_iter()
                    .filter_map(|lifecycle| {
                        lifecycle
                            .approval
                            .into_timeline(workspace_id, renderer_session_id, &run.id)
                            .ok()
                            .map(|event| DesktopRunApprovalLifecycleSnapshot {
                                event,
                                state: lifecycle.state,
                            })
                    })
                    .collect(),
            },
            terminal_events,
            settled: stream.projection.is_settled(),
        })
    }
}

fn settled_terminal_reattach_snapshot(
    run: &DesktopRunSnapshot,
) -> Option<DesktopRunProjectionSnapshot> {
    if !run.status.is_terminal()
        || run.terminal_tasks.iter().any(|task| {
            !matches!(
                &task.status,
                DesktopTerminalTaskStatus::Exited { .. }
                    | DesktopTerminalTaskStatus::Failed { .. }
                    | DesktopTerminalTaskStatus::Cancelled
                    | DesktopTerminalTaskStatus::Interrupted
            )
        })
    {
        return None;
    }
    Some(DesktopRunProjectionSnapshot {
        events: Vec::new(),
        has_gap: run.stream_sequence > 0,
        stream_state: DesktopRunStreamState::Terminal,
        stream_message: Some("Run reconciled from the server snapshot."),
    })
}

impl RunProjection {
    fn accept_stream_event(&mut self, event: &DesktopTimelineEvent) -> bool {
        if is_execution_preview(event)
            && event.execution_id.as_ref().is_some_and(|execution| {
                self.terminal_tasks
                    .get(execution)
                    .and_then(terminal_execution_id)
                    .is_some()
            })
        {
            return false;
        }
        self.event_cursor.accept(event)
    }

    fn new(run_status: DesktopRunStatus, has_gap: bool) -> Self {
        Self {
            event_cursor: RunEventCursor::default(),
            events: VecDeque::new(),
            event_text_bytes: 0,
            pending_approvals: BTreeMap::new(),
            terminal_tasks: BTreeMap::new(),
            has_gap,
            last_sequence: 0,
            last_replay_id: None,
            last_registry_revision: 0,
            stream_state: if run_status.is_terminal() {
                DesktopRunStreamState::Terminal
            } else {
                DesktopRunStreamState::Connecting
            },
            stream_message: None,
            run_status,
            task_pause_observed: run_status == DesktopRunStatus::Paused,
        }
    }

    fn reconcile_run_snapshot(
        &mut self,
        run: &DesktopRunSnapshot,
        workspace_id: &str,
        renderer_session_id: &str,
    ) -> bool {
        if run.stream_sequence < self.last_registry_revision {
            return false;
        }
        if run.pending_approvals.len() > MAX_PENDING_APPROVALS {
            self.has_gap = true;
            return false;
        }
        let mut canonical = BTreeMap::new();
        for pending in run.pending_approvals.iter().cloned() {
            let Ok(event) = pending.into_timeline(workspace_id, renderer_session_id, &run.id)
            else {
                self.has_gap = true;
                return false;
            };
            let Some(call_id) = event.item_id.clone() else {
                self.has_gap = true;
                return false;
            };
            canonical.insert(call_id, event);
        }
        let mut canonical_terminal_tasks = BTreeMap::new();
        for task in &run.terminal_tasks {
            let Ok(task) = DesktopTimelineTerminalTask::try_from(task) else {
                self.has_gap = true;
                return false;
            };
            let event = terminal_snapshot_timeline(
                workspace_id,
                renderer_session_id,
                &run.id,
                run.stream_sequence,
                task,
            );
            canonical_terminal_tasks.insert(event.item_id.clone().unwrap_or_default(), event);
        }
        self.pending_approvals = canonical;
        self.terminal_tasks = canonical_terminal_tasks;
        for event in self.terminal_tasks.values() {
            self.event_cursor.retire_terminal_execution(event, false);
            self.events
                .retain(|old| retain_preview_after_event(old, event));
        }
        self.event_text_bytes = self.events.iter().map(event_text_bytes).sum();
        self.last_registry_revision = run.stream_sequence;
        self.run_status = run.status;
        true
    }

    fn push(&mut self, event: DesktopTimelineEvent) {
        if let Some(preview) = &event.live_preview {
            let execution = is_execution_preview(&event);
            self.events.retain(|old| {
                old.live_preview.as_ref().is_none_or(|old_preview| {
                    if is_execution_preview(old) != execution {
                        return true;
                    }
                    if execution {
                        return old.execution_id != event.execution_id;
                    }
                    old_preview.attempt_id == preview.attempt_id
                        && old_preview.slot_id != preview.slot_id
                })
            });
            // Four execution snapshots and four provider snapshots have separate eviction budgets.
            while self
                .events
                .iter()
                .filter(|old| old.live_preview.is_some() && is_execution_preview(old) == execution)
                .count()
                >= 4
            {
                if let Some(index) = self.events.iter().position(|old| {
                    old.live_preview.is_some() && is_execution_preview(old) == execution
                }) {
                    self.events.remove(index);
                }
            }
        } else {
            self.events
                .retain(|old| retain_preview_after_event(old, &event));
        }
        self.event_text_bytes = self.events.iter().map(event_text_bytes).sum();
        if event.replayable {
            self.last_sequence = self.last_sequence.max(event.sequence);
        }
        if let Some(replay_id) = event.replay_id.as_ref() {
            self.last_replay_id = Some(replay_id.clone());
        }
        if self.events.iter().any(|current| {
            event.live_preview.is_none()
                && current.live_preview.is_none()
                && event_identity(current) == event_identity(&event)
        }) {
            return;
        }
        match event.kind {
            DesktopTimelineEventKind::TerminalLifecycle => {
                if let Some(task) = event.terminal_task.as_ref() {
                    let replace = self
                        .terminal_tasks
                        .get(&task.task_id)
                        .and_then(|current| current.terminal_task.as_ref())
                        .is_none_or(|current| current.generation < task.generation);
                    if replace {
                        self.terminal_tasks
                            .insert(task.task_id.clone(), event.clone());
                    }
                } else {
                    self.has_gap = true;
                }
            }
            DesktopTimelineEventKind::ApprovalRequested => {
                if let Some(item_id) = event.item_id.as_ref() {
                    self.pending_approvals
                        .insert(item_id.clone(), event.clone());
                    while self.pending_approvals.len() > MAX_PENDING_APPROVALS {
                        let oldest = self
                            .pending_approvals
                            .iter()
                            .min_by_key(|(_, pending)| pending.sequence)
                            .map(|(item_id, _)| item_id.clone());
                        if let Some(item_id) = oldest {
                            self.pending_approvals.remove(&item_id);
                            self.has_gap = true;
                        }
                    }
                }
            }
            DesktopTimelineEventKind::ApprovalResolved => {
                if let Some(item_id) = event.item_id.as_ref() {
                    self.pending_approvals.remove(item_id);
                }
            }
            DesktopTimelineEventKind::TaskRunFinished
                if event.status.as_deref() == Some("paused") =>
            {
                self.task_pause_observed = true;
            }
            DesktopTimelineEventKind::RunFinished => {
                self.run_status = DesktopRunStatus::Finished;
            }
            DesktopTimelineEventKind::RunFailed => {
                self.run_status = DesktopRunStatus::Failed;
            }
            DesktopTimelineEventKind::RunBlocked => {
                self.run_status = DesktopRunStatus::Blocked;
            }
            DesktopTimelineEventKind::RunPaused => {
                self.run_status = DesktopRunStatus::Paused;
            }
            DesktopTimelineEventKind::RunInterrupted => {
                self.run_status = DesktopRunStatus::Interrupted;
            }
            DesktopTimelineEventKind::RunCancelled => {
                self.run_status =
                    if self.task_pause_observed || event.status.as_deref() == Some("paused") {
                        DesktopRunStatus::Paused
                    } else {
                        DesktopRunStatus::Cancelled
                    };
            }
            _ => {}
        }
        self.event_text_bytes = self
            .event_text_bytes
            .saturating_add(event_text_bytes(&event));
        self.events.push_back(event);
        while self.events.len() > MAX_ATTACHMENT_EVENTS
            || self.event_text_bytes > MAX_ATTACHMENT_TEXT_BYTES
        {
            let Some(removed) = self.events.pop_front() else {
                break;
            };
            self.event_text_bytes = self
                .event_text_bytes
                .saturating_sub(event_text_bytes(&removed));
            self.has_gap = true;
        }
    }

    fn snapshot(&self) -> DesktopRunProjectionSnapshot {
        let mut events = self.events.iter().cloned().collect::<Vec<_>>();
        for pending in self.pending_approvals.values() {
            if !events
                .iter()
                .any(|event| event_identity(event) == event_identity(pending))
            {
                events.push(pending.clone());
            }
        }
        for terminal in self.terminal_tasks.values() {
            if !events
                .iter()
                .any(|event| event_identity(event) == event_identity(terminal))
            {
                events.push(terminal.clone());
            }
        }
        events.sort_by_key(|event| event.sequence);
        DesktopRunProjectionSnapshot {
            events,
            has_gap: self.has_gap,
            stream_state: self.stream_state,
            stream_message: self.stream_message,
        }
    }

    fn is_settled(&self) -> bool {
        self.run_status.is_terminal()
            && self.terminal_tasks.values().all(|event| {
                event.terminal_task.as_ref().is_none_or(|task| {
                    matches!(
                        task.status.as_str(),
                        "exited" | "failed" | "cancelled" | "interrupted"
                    )
                })
            })
    }
}

async fn follow_run(
    owner: DesktopRunStreamOwner,
    app: AppHandle,
    client: DesktopHttpClient,
    workspace_id: String,
    renderer_session_id: String,
    durable_session_id: String,
    owner_revision: String,
    initial_run: DesktopRunSnapshot,
    mut cursor: Option<String>,
) {
    let run_id = initial_run.id.clone();
    publish_status(
        &owner,
        &app,
        &workspace_id,
        &renderer_session_id,
        &run_id,
        DesktopRunStreamState::Connecting,
        None,
    )
    .await;
    let mut attempts = 0_u8;
    loop {
        let connection = client
            .run_events(
                &renderer_session_id,
                &durable_session_id,
                &run_id,
                &owner_revision,
                cursor.as_deref(),
            )
            .await;
        let mut stream = match connection {
            Ok(stream) => {
                publish_status(
                    &owner,
                    &app,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                    DesktopRunStreamState::Live,
                    None,
                )
                .await;
                stream
            }
            Err(_) => {
                if terminal_snapshot(
                    &owner,
                    &app,
                    &client,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                )
                .await
                {
                    return;
                }
                attempts = attempts.saturating_add(1);
                if attempts >= MAX_RECONNECT_ATTEMPTS {
                    publish_status(
                        &owner,
                        &app,
                        &workspace_id,
                        &renderer_session_id,
                        &run_id,
                        DesktopRunStreamState::Error,
                        Some("Run updates are unavailable. Reopen the workspace to reconcile."),
                    )
                    .await;
                    return;
                }
                publish_status(
                    &owner,
                    &app,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                    DesktopRunStreamState::Reconnecting,
                    Some("Reconnecting from the last durable event…"),
                )
                .await;
                tokio::time::sleep(reconnect_delay(attempts)).await;
                continue;
            }
        };
        let connected_at = Instant::now();
        let mut observed_event = false;
        loop {
            let next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next_event()).await;
            let protocol_event = match next {
                Ok(Ok(Some(event))) => event,
                Ok(Err(sigil_desktop::DesktopClientError::EventStreamGap)) => {
                    // A gap is a wake to rebuild from server-owned durable truth. Discard the
                    // incremental cursor and replay the canonical bounded run stream; projection
                    // identity de-duplication makes already-seen events idempotent.
                    if let Ok(snapshot) = client.run(&run_id).await
                        && let Some(reconciliation) = owner
                            .reconcile_run_snapshot(&workspace_id, &renderer_session_id, &snapshot)
                            .await
                    {
                        let _ = app.emit(
                            DESKTOP_RUN_APPROVAL_SNAPSHOT_NAME,
                            reconciliation.approval_snapshot,
                        );
                        for event in reconciliation.terminal_events {
                            let _ = app.emit(DESKTOP_RUN_EVENT_NAME, event);
                        }
                        if reconciliation.settled {
                            publish_status(
                                &owner,
                                &app,
                                &workspace_id,
                                &renderer_session_id,
                                &run_id,
                                DesktopRunStreamState::Terminal,
                                Some("Run and background terminal tasks reconciled from the server snapshot."),
                            )
                            .await;
                            return;
                        }
                    }
                    cursor = None;
                    owner.reset_event_cursor(&workspace_id, &run_id).await;
                    publish_status(
                        &owner,
                        &app,
                        &workspace_id,
                        &renderer_session_id,
                        &run_id,
                        DesktopRunStreamState::Reconnecting,
                        Some("Refreshing run state after a live event gap…"),
                    )
                    .await;
                    break;
                }
                Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
            };
            observed_event = true;
            attempts = 0;
            let durable_cursor = protocol_event.replay_id.clone();
            let timeline = match protocol_event.into_timeline(
                &workspace_id,
                &durable_session_id,
                &run_id,
                &renderer_session_id,
            ) {
                Ok(event) => event,
                Err(_) => break,
            };
            let Some(settled) = owner.record_stream_event(timeline.clone()).await else {
                continue;
            };
            if app.emit(DESKTOP_RUN_EVENT_NAME, timeline).is_err() {
                return;
            }
            if let Some(replay_id) = durable_cursor {
                cursor = Some(replay_id);
            }
            if settled {
                publish_status(
                    &owner,
                    &app,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                    DesktopRunStreamState::Terminal,
                    None,
                )
                .await;
                return;
            }
        }
        if terminal_snapshot(
            &owner,
            &app,
            &client,
            &workspace_id,
            &renderer_session_id,
            &run_id,
        )
        .await
        {
            return;
        }
        attempts = next_reconnect_attempt(attempts, connected_at.elapsed(), observed_event);
        if attempts >= MAX_RECONNECT_ATTEMPTS {
            publish_status(
                &owner,
                &app,
                &workspace_id,
                &renderer_session_id,
                &run_id,
                DesktopRunStreamState::Error,
                Some("Run updates repeatedly disconnected. Reopen the workspace to reconcile."),
            )
            .await;
            return;
        }
        publish_status(
            &owner,
            &app,
            &workspace_id,
            &renderer_session_id,
            &run_id,
            DesktopRunStreamState::Reconnecting,
            Some("Live progress paused; replaying durable events…"),
        )
        .await;
        tokio::time::sleep(reconnect_delay(attempts)).await;
    }
}

async fn terminal_snapshot(
    owner: &DesktopRunStreamOwner,
    app: &AppHandle,
    client: &DesktopHttpClient,
    workspace_id: &str,
    renderer_session_id: &str,
    run_id: &str,
) -> bool {
    let Ok(snapshot) = client.run(run_id).await else {
        return false;
    };
    let Some(reconciliation) = owner
        .reconcile_run_snapshot(workspace_id, renderer_session_id, &snapshot)
        .await
    else {
        return false;
    };
    let _ = app.emit(
        DESKTOP_RUN_APPROVAL_SNAPSHOT_NAME,
        reconciliation.approval_snapshot,
    );
    for event in reconciliation.terminal_events {
        let _ = app.emit(DESKTOP_RUN_EVENT_NAME, event);
    }
    if !snapshot.status.is_terminal() || !reconciliation.settled {
        return false;
    }
    let Some((kind, status)) = terminal_timeline_projection(snapshot.status) else {
        return false;
    };
    let timeline = DesktopTimelineEvent {
        workspace_id: workspace_id.to_owned(),
        session_id: renderer_session_id.to_owned(),
        run_id: run_id.to_owned(),
        sequence: snapshot.stream_sequence,
        run_sequence: snapshot.stream_sequence.to_string(),
        replayable: false,
        replay_id: None,
        provisional_id: None,
        live_preview: None,
        kind,
        text: None,
        item_id: None,
        tool_name: None,
        status: Some(status.to_owned()),
        assistant_kind: None,
        tool_input: None,
        execution_id: None,
        execution_started_at_ms: None,
        execution_updated_at_ms: None,
        display_call_id: None,
        approval: None,
        approval_request_id: None,
        tool_execution: None,
        task: None,
        terminal_task: None,
        provider_turn_recovery: None,
        route_recovery: None,
        route_transition: None,
    };
    owner.record_event(timeline.clone()).await;
    let _ = app.emit(DESKTOP_RUN_EVENT_NAME, timeline);
    publish_status(
        owner,
        app,
        workspace_id,
        renderer_session_id,
        run_id,
        DesktopRunStreamState::Terminal,
        Some("Run reconciled from the server snapshot."),
    )
    .await;
    true
}

fn terminal_snapshot_timeline(
    workspace_id: &str,
    renderer_session_id: &str,
    run_id: &str,
    sequence: u64,
    task: DesktopTimelineTerminalTask,
) -> DesktopTimelineEvent {
    DesktopTimelineEvent {
        workspace_id: workspace_id.to_owned(),
        session_id: renderer_session_id.to_owned(),
        run_id: run_id.to_owned(),
        sequence,
        run_sequence: sequence.to_string(),
        replayable: false,
        replay_id: None,
        provisional_id: None,
        live_preview: None,
        kind: DesktopTimelineEventKind::TerminalLifecycle,
        text: None,
        item_id: Some(task.task_id.clone()),
        tool_name: None,
        status: Some(task.status.clone()),
        assistant_kind: None,
        tool_input: None,
        execution_id: Some(task.task_id.clone()),
        execution_started_at_ms: None,
        execution_updated_at_ms: Some(task.emitted_at_ms),
        display_call_id: None,
        approval: None,
        approval_request_id: None,
        tool_execution: None,
        task: None,
        terminal_task: Some(task),
        provider_turn_recovery: None,
        route_recovery: None,
        route_transition: None,
    }
}

async fn publish_status(
    owner: &DesktopRunStreamOwner,
    app: &AppHandle,
    workspace_id: &str,
    session_id: &str,
    run_id: &str,
    state: DesktopRunStreamState,
    message: Option<&'static str>,
) {
    owner
        .record_status(workspace_id, run_id, state, message)
        .await;
    emit_status(app, workspace_id, session_id, run_id, state, message);
}

fn emit_status(
    app: &AppHandle,
    workspace_id: &str,
    session_id: &str,
    run_id: &str,
    state: DesktopRunStreamState,
    message: Option<&'static str>,
) {
    let _ = app.emit(
        DESKTOP_RUN_STREAM_STATUS_NAME,
        DesktopRunStreamStatus {
            workspace_id: workspace_id.to_owned(),
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            state,
            message,
        },
    );
}

fn reconnect_delay(attempt: u8) -> Duration {
    Duration::from_millis(250_u64.saturating_mul(1_u64 << attempt.min(3)))
}

fn next_reconnect_attempt(
    previous_attempts: u8,
    connected_for: Duration,
    observed_event: bool,
) -> u8 {
    if observed_event || connected_for >= MIN_HEALTHY_STREAM_LIFETIME {
        1
    } else {
        previous_attempts.saturating_add(1)
    }
}

fn stream_key(workspace_id: &str, run_id: &str) -> String {
    format!("{workspace_id}:{run_id}")
}

fn event_identity(event: &DesktopTimelineEvent) -> (u64, DesktopTimelineEventKind, Option<&str>) {
    (event.sequence, event.kind, event.item_id.as_deref())
}

fn event_text_bytes(event: &DesktopTimelineEvent) -> usize {
    let approval_bytes = event.approval.as_ref().map_or(0, |approval| {
        approval.preview_title.as_ref().map_or(0, String::len)
            + approval.preview_summary.as_ref().map_or(0, String::len)
            + approval.preview_body.as_ref().map_or(0, String::len)
    });
    event.text.as_ref().map_or(0, String::len)
        + event.tool_input.as_ref().map_or(0, String::len)
        + approval_bytes
}

fn terminal_status(status: DesktopRunStatus) -> DesktopRunStatus {
    if status.is_terminal() {
        status
    } else {
        DesktopRunStatus::Interrupted
    }
}

fn terminal_timeline_projection(
    status: DesktopRunStatus,
) -> Option<(DesktopTimelineEventKind, &'static str)> {
    match status {
        DesktopRunStatus::Finished => Some((DesktopTimelineEventKind::RunFinished, "finished")),
        DesktopRunStatus::Failed => Some((DesktopTimelineEventKind::RunFailed, "failed")),
        DesktopRunStatus::Blocked => Some((DesktopTimelineEventKind::RunBlocked, "blocked")),
        DesktopRunStatus::Interrupted => {
            Some((DesktopTimelineEventKind::RunInterrupted, "interrupted"))
        }
        DesktopRunStatus::Cancelled => Some((DesktopTimelineEventKind::RunCancelled, "cancelled")),
        DesktopRunStatus::Paused => Some((DesktopTimelineEventKind::RunPaused, "paused")),
        _ => None,
    }
}

#[cfg(test)]
#[path = "tests/run_streams_tests.rs"]
mod tests;
