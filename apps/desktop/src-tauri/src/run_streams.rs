use std::{collections::BTreeMap, sync::Arc, time::Duration};

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

#[derive(Debug, Clone, Copy, Serialize)]
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
    workspace_id: String,
    session_id: String,
    run_id: String,
    state: DesktopRunStreamState,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'static str>,
}

struct OwnedRunStream {
    workspace_id: String,
    task: JoinHandle<()>,
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
        run: DesktopRunSnapshot,
    ) {
        let run_id = run.id.clone();
        let key = stream_key(&workspace_id, &run_id);
        let task = tauri::async_runtime::spawn(follow_run(
            app,
            client,
            workspace_id.clone(),
            renderer_session_id,
            durable_session_id,
            run,
        ));
        let mut streams = self.streams.lock().await;
        streams.retain(|_, stream| !stream.task.inner().is_finished());
        if let Some(previous) = streams.insert(key, OwnedRunStream { workspace_id, task }) {
            previous.task.abort();
        }
    }

    pub(crate) async fn stop_workspace(&self, workspace_id: &str) {
        let mut streams = self.streams.lock().await;
        let keys = streams
            .iter()
            .filter(|(_, stream)| stream.workspace_id == workspace_id)
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in keys {
            if let Some(stream) = streams.remove(&key) {
                stream.task.abort();
            }
        }
    }

    pub(crate) async fn stop_all(&self) {
        let streams = std::mem::take(&mut *self.streams.lock().await);
        for stream in streams.into_values() {
            stream.task.abort();
        }
    }
}

async fn follow_run(
    app: AppHandle,
    client: DesktopHttpClient,
    workspace_id: String,
    renderer_session_id: String,
    durable_session_id: String,
    initial_run: DesktopRunSnapshot,
) {
    let run_id = initial_run.id.clone();
    emit_status(
        &app,
        &workspace_id,
        &renderer_session_id,
        &run_id,
        DesktopRunStreamState::Connecting,
        None,
    );
    let mut cursor = None::<String>;
    let mut last_sequence = 0_u64;
    let mut attempts = 0_u8;
    loop {
        let connection = client
            .run_events(&durable_session_id, &run_id, cursor.as_deref())
            .await;
        let mut stream = match connection {
            Ok(stream) => {
                emit_status(
                    &app,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                    DesktopRunStreamState::Live,
                    None,
                );
                stream
            }
            Err(_) => {
                if terminal_snapshot(&app, &client, &workspace_id, &renderer_session_id, &run_id)
                    .await
                {
                    return;
                }
                attempts = attempts.saturating_add(1);
                if attempts >= MAX_RECONNECT_ATTEMPTS {
                    emit_status(
                        &app,
                        &workspace_id,
                        &renderer_session_id,
                        &run_id,
                        DesktopRunStreamState::Error,
                        Some("Run updates are unavailable. Reopen the workspace to reconcile."),
                    );
                    return;
                }
                emit_status(
                    &app,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                    DesktopRunStreamState::Reconnecting,
                    Some("Reconnecting from the last durable event…"),
                );
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
            if app.emit(DESKTOP_RUN_EVENT_NAME, timeline).is_err() {
                return;
            }
            if let Some(replay_id) = durable_cursor {
                cursor = Some(replay_id);
            }
            if terminal {
                emit_status(
                    &app,
                    &workspace_id,
                    &renderer_session_id,
                    &run_id,
                    DesktopRunStreamState::Terminal,
                    None,
                );
                return;
            }
        }
        if terminal_snapshot(&app, &client, &workspace_id, &renderer_session_id, &run_id).await {
            return;
        }
        attempts = attempts.saturating_add(1);
        if attempts >= MAX_RECONNECT_ATTEMPTS {
            emit_status(
                &app,
                &workspace_id,
                &renderer_session_id,
                &run_id,
                DesktopRunStreamState::Error,
                Some("Run updates repeatedly disconnected. Reopen the workspace to reconcile."),
            );
            return;
        }
        emit_status(
            &app,
            &workspace_id,
            &renderer_session_id,
            &run_id,
            DesktopRunStreamState::Reconnecting,
            Some("Live progress paused; replaying durable events…"),
        );
        tokio::time::sleep(reconnect_delay(attempts)).await;
    }
}

async fn terminal_snapshot(
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
    let _ = app.emit(
        DESKTOP_RUN_EVENT_NAME,
        DesktopTimelineEvent {
            workspace_id: workspace_id.to_owned(),
            session_id: renderer_session_id.to_owned(),
            run_id: run_id.to_owned(),
            sequence: snapshot.stream_sequence,
            replayable: false,
            replay_id: None,
            kind,
            text: None,
            item_id: None,
            tool_name: None,
            status: Some(status.to_owned()),
            approval: None,
        },
    );
    emit_status(
        app,
        workspace_id,
        renderer_session_id,
        run_id,
        DesktopRunStreamState::Terminal,
        Some("Run reconciled from the server snapshot."),
    );
    true
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

#[cfg(test)]
#[path = "tests/run_streams_tests.rs"]
mod tests;
