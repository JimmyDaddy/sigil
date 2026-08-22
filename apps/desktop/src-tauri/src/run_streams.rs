use std::{
    collections::{BTreeMap, VecDeque},
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
            let initial_sequence = stream.projection.last_sequence;
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
                initial_sequence,
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
    fn new(run_status: DesktopRunStatus, has_gap: bool) -> Self {
        Self {
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
        self.last_registry_revision = run.stream_sequence;
        self.run_status = run.status;
        true
    }

    fn push(&mut self, event: DesktopTimelineEvent) {
        if self
            .events
            .iter()
            .any(|current| event_identity(current) == event_identity(&event))
        {
            return;
        }
        self.last_sequence = self.last_sequence.max(event.sequence);
        if let Some(replay_id) = event.replay_id.as_ref() {
            self.last_replay_id = Some(replay_id.clone());
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
    mut last_sequence: u64,
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
                    last_sequence = 0;
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
            if protocol_event.run_event.sequence <= last_sequence {
                continue;
            }
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
            last_sequence = timeline.sequence;
            let settled = owner.record_event(timeline.clone()).await;
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
        kind,
        text: None,
        item_id: None,
        tool_name: None,
        status: Some(status.to_owned()),
        assistant_kind: None,
        tool_input: None,
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
        kind: DesktopTimelineEventKind::TerminalLifecycle,
        text: None,
        item_id: Some(task.task_id.clone()),
        tool_name: None,
        status: Some(task.status.clone()),
        assistant_kind: None,
        tool_input: None,
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
    event.text.as_ref().map_or(0, String::len) + approval_bytes
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
