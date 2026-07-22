use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use serde::Serialize;
use sigil_desktop::{
    DesktopHttpClient, DesktopRunSnapshot, DesktopRunStatus, DesktopTimelineEvent,
    DesktopTimelineEventKind,
};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter};
use tokio::sync::Mutex;

pub(crate) const DESKTOP_RUN_EVENT_NAME: &str = "sigil-run-event";
pub(crate) const DESKTOP_RUN_STREAM_STATUS_NAME: &str = "sigil-run-stream-status";

const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
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
    has_gap: bool,
    last_sequence: u64,
    last_replay_id: Option<String>,
    stream_state: DesktopRunStreamState,
    stream_message: Option<&'static str>,
    run_status: DesktopRunStatus,
}

pub(crate) struct DesktopRunProjectionSnapshot {
    pub(crate) events: Vec<DesktopTimelineEvent>,
    pub(crate) has_gap: bool,
    pub(crate) stream_state: DesktopRunStreamState,
    pub(crate) stream_message: Option<&'static str>,
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
                || !stream.projection.run_status.is_terminal()
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

        let follower_finished = stream
            .task
            .as_ref()
            .is_none_or(|task| task.inner().is_finished());
        if run.status.is_terminal() {
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

    async fn record_event(&self, event: DesktopTimelineEvent) {
        let key = stream_key(&event.workspace_id, &event.run_id);
        let mut streams = self.streams.lock().await;
        let Some(stream) = streams.get_mut(&key) else {
            return;
        };
        stream.projection.push(event);
    }
}

impl RunProjection {
    fn new(run_status: DesktopRunStatus, has_gap: bool) -> Self {
        Self {
            events: VecDeque::new(),
            event_text_bytes: 0,
            pending_approvals: BTreeMap::new(),
            has_gap,
            last_sequence: 0,
            last_replay_id: None,
            stream_state: if run_status.is_terminal() {
                DesktopRunStreamState::Terminal
            } else {
                DesktopRunStreamState::Connecting
            },
            stream_message: None,
            run_status,
        }
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
            DesktopTimelineEventKind::RunFinished => {
                self.run_status = DesktopRunStatus::Finished;
            }
            DesktopTimelineEventKind::RunFailed => {
                self.run_status = DesktopRunStatus::Failed;
            }
            DesktopTimelineEventKind::RunCancelled => {
                self.run_status = DesktopRunStatus::Cancelled;
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
        events.sort_by_key(|event| event.sequence);
        DesktopRunProjectionSnapshot {
            events,
            has_gap: self.has_gap,
            stream_state: self.stream_state,
            stream_message: self.stream_message,
        }
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
        loop {
            let next = tokio::time::timeout(STREAM_IDLE_TIMEOUT, stream.next_event()).await;
            let protocol_event = match next {
                Ok(Ok(Some(event))) => event,
                Ok(Ok(None)) | Ok(Err(_)) | Err(_) => break,
            };
            if protocol_event.run_event.sequence <= last_sequence {
                continue;
            }
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
            let terminal = timeline_is_terminal(&timeline);
            owner.record_event(timeline.clone()).await;
            if app.emit(DESKTOP_RUN_EVENT_NAME, timeline).is_err() {
                return;
            }
            if let Some(replay_id) = durable_cursor {
                cursor = Some(replay_id);
            }
            if terminal {
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
        attempts = attempts.saturating_add(1);
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
    if !snapshot.status.is_terminal() {
        return false;
    }
    let (kind, status) = match snapshot.status {
        DesktopRunStatus::Finished => (DesktopTimelineEventKind::RunFinished, "finished"),
        DesktopRunStatus::Failed | DesktopRunStatus::Interrupted => {
            (DesktopTimelineEventKind::RunFailed, "failed")
        }
        DesktopRunStatus::Cancelled => (DesktopTimelineEventKind::RunCancelled, "cancelled"),
        _ => return false,
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

fn timeline_is_terminal(event: &DesktopTimelineEvent) -> bool {
    matches!(
        event.kind,
        DesktopTimelineEventKind::RunFinished
            | DesktopTimelineEventKind::RunFailed
            | DesktopTimelineEventKind::RunCancelled
    )
}

fn reconnect_delay(attempt: u8) -> Duration {
    Duration::from_millis(250_u64.saturating_mul(1_u64 << attempt.min(3)))
}

fn stream_key(workspace_id: &str, run_id: &str) -> String {
    format!("{workspace_id}:{run_id}")
}

fn event_identity(event: &DesktopTimelineEvent) -> (u64, DesktopTimelineEventKind) {
    (event.sequence, event.kind)
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

#[cfg(test)]
#[path = "tests/run_streams_tests.rs"]
mod tests;
