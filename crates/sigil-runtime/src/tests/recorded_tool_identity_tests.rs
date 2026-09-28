use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, StreamExt, stream};
use sigil_kernel::{
    Agent, AgentRunInput, AgentRunOptions, CompactionConfig, CompletionRequest, InteractionMode,
    JsonlSessionStore, MemoryConfig, PermissionConfig, PermissionEvaluationContext, Provider,
    ProviderCapabilities, ProviderChunk, PublicEventOutboxProjectionV1, PublicRunEventKind,
    ReasoningStreamSupport, Session, SessionLogEntry, ToolCall, ToolRegistry,
};
use tokio::sync::Notify;

struct RepeatedToolProvider {
    turns: AtomicUsize,
    preview_ready: Arc<Notify>,
    release_duplicate: Arc<Notify>,
}

#[async_trait]
impl Provider for RepeatedToolProvider {
    fn name(&self) -> &str {
        "repeated-tool-fixture"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Unsupported,
            supports_reasoning_effort: false,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }
    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if self.turns.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "previous-call".into(),
                    name: "unregistered_fixture_tool".into(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "previous-call".into(),
                    delta: "{}".into(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "previous-call".into(),
                    name: "unregistered_fixture_tool".into(),
                    args_json: "{}".into(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        let ready = Arc::clone(&self.preview_ready);
        let release = Arc::clone(&self.release_duplicate);
        Ok(Box::pin(
            stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "valid transient text before bad identity".into(),
                )),
                Ok(ProviderChunk::ToolCallStart {
                    id: "fresh-preview".into(),
                    name: "unregistered_fixture_tool".into(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "fresh-preview".into(),
                    delta: "{}".into(),
                }),
            ])
            .chain(stream::once(async move {
                ready.notify_one();
                release.notified().await;
                Ok(ProviderChunk::ToolCallStart {
                    id: "previous-call".into(),
                    name: "unregistered_fixture_tool".into(),
                })
            }))
            .chain(stream::iter(vec![Ok(ProviderChunk::Done)])),
        ))
    }
}

#[tokio::test]
async fn recorded_tool_identity_failure_retires_actual_previews_and_publishes_terminal()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("repeated-tool-fixture", "model", store.clone())?;
    let mut recorder =
        crate::ApplicationRunEventRecorder::start(&session, "identity-public-run", "inspect")?;
    let source = recorder.live_preview_source();
    let ready = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let agent = Agent::new(
        RepeatedToolProvider {
            turns: AtomicUsize::new(0),
            preview_ready: Arc::clone(&ready),
            release_duplicate: Arc::clone(&release),
        },
        ToolRegistry::new(),
    );
    let error = {
        let run = agent.run_with_input(
            &mut session,
            AgentRunInput::user("inspect").with_logical_run_id("identity-public-run"),
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: PermissionEvaluationContext::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut recorder,
        );
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("run ended before the real preview seam: {result:?}"),
            _ = ready.notified() => {}
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("provider never reached its preview seam"),
        }
        let previews = source.reader().poll_updates()?;
        assert!(
            previews
                .iter()
                .any(|view| view.preview.as_str().contains("valid transient text"))
        );
        assert!(
            previews
                .iter()
                .any(|view| view.kind == sigil_application::LiveRunUpdateKind::ToolCallArguments)
        );
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(5), run)
            .await?
            .expect_err("recorded identity must fail before a second durable preamble")
    };
    assert!(error.is::<sigil_kernel::ProviderTurnRecoveryTerminalError>());
    assert!(
        source.reader().poll_updates()?.is_empty(),
        "partial discard retires all actual previews before terminal"
    );
    recorder.finish_error(&error)?;
    assert!(source.is_terminal());
    assert!(source.reader().poll_updates()?.is_empty());
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(entry, SessionLogEntry::ToolResultV3(_)))
            .count(),
        1
    );
    assert!(session.active_projection_snapshot()?.is_some());
    let records = store.read_event_records_writer()?;
    let outbox = PublicEventOutboxProjectionV1::from_records(&records)?;
    let events = outbox.events_in_order();
    assert_eq!(
        events
            .iter()
            .filter(|entry| matches!(&entry.event.event,
                PublicRunEventKind::ToolCallStarted { call } if call.id == "previous-call"
            ))
            .count(),
        1,
        "a retired public tool card cannot receive a second start"
    );
    assert_eq!(
        events
            .iter()
            .filter(|entry| matches!(&entry.event.event,
                PublicRunEventKind::ToolCallCompleted { call } if call.id == "previous-call"
            ))
            .count(),
        1
    );
    assert!(events.iter().any(|entry| matches!(
        &entry.event.event,
        PublicRunEventKind::ProviderTurnPartialOutputDiscarded { .. }
    )));
    assert!(matches!(
        &events.last().expect("actual public terminal").event.event,
        PublicRunEventKind::RunBlocked { .. }
            | PublicRunEventKind::RunPaused { .. }
            | PublicRunEventKind::RunFailed { .. }
    ));
    drop(recorder);
    drop(session);
    let restored = Session::load_from_store("repeated-tool-fixture", "model", store)?;
    let restored_recorder =
        crate::ApplicationRunEventRecorder::resume(&restored, "identity-public-run")?;
    assert!(restored_recorder.live_preview_source().is_terminal());
    assert!(restored.active_projection_snapshot()?.is_some());
    Ok(())
}
