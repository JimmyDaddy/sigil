use super::*;

struct ToolIdentityProvider {
    turns: Mutex<VecDeque<Vec<ProviderChunk>>>,
}

#[async_trait]
impl Provider for ToolIdentityProvider {
    fn name(&self) -> &str {
        "tool-identity-fixture"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let chunks = self
            .turns
            .lock()
            .expect("fixture turns")
            .pop_front()
            .unwrap_or_else(|| {
                vec![
                    ProviderChunk::TextDelta("continued safely".into()),
                    ProviderChunk::Done,
                ]
            });
        Ok(Box::pin(stream::iter(chunks.into_iter().map(Ok))))
    }
}

fn call_chunks(id: &str, streamed: bool) -> Vec<ProviderChunk> {
    let call = ToolCall {
        id: id.into(),
        name: "echo".into(),
        args_json: "{}".into(),
    };
    let mut chunks = Vec::new();
    if streamed {
        chunks.push(ProviderChunk::ToolCallStart {
            id: call.id.clone(),
            name: call.name.clone(),
        });
        chunks.push(ProviderChunk::ToolCallArgsDelta {
            id: call.id.clone(),
            delta: call.args_json.clone(),
        });
    }
    chunks.push(ProviderChunk::ToolCallComplete(call));
    chunks
}

fn identity_agent(
    turns: Vec<Vec<ProviderChunk>>,
    probe: &Arc<ToolSchedulerProbe>,
) -> Agent<ToolIdentityProvider> {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ScheduledReadTool {
        name: "echo".into(),
        delay: Duration::ZERO,
        parallel: false,
        mutation_tracking: ToolMutationTracking::None,
        fail: false,
        probe: Arc::clone(probe),
    }));
    Agent::new(
        ToolIdentityProvider {
            turns: Mutex::new(turns.into()),
        },
        tools,
    )
}

async fn repeated_recorded_call_is_rejected_before_publication(streamed: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::load_from_store("tool-identity-fixture", "model", store.clone())?;
    let probe = Arc::new(ToolSchedulerProbe::default());
    let mut first = call_chunks("existing-call", true);
    first.push(ProviderChunk::Done);
    let mut second = vec![ProviderChunk::TextDelta("uncommitted partial".into())];
    second.extend(call_chunks("fresh-preview-only", true));
    second.extend(call_chunks("existing-call", streamed));
    second.push(ProviderChunk::Done);
    let agent = identity_agent(vec![first, second], &probe);
    let mut handler = RecordingEventHandler::default();
    let error = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect").with_logical_run_id("identity-first-run"),
            scripted_run_options(4),
            &mut handler,
        )
        .await
        .expect_err("recorded call identity must be rejected before another effect or preamble");
    assert!(error.chain().any(|cause| {
        cause
            .to_string()
            .contains("provider reused a recorded tool-call id")
    }));
    assert!(error.is::<crate::ProviderTurnRecoveryTerminalError>());
    assert_eq!(
        *probe.events.lock().expect("execution events"),
        ["start:echo", "end:echo"]
    );
    assert_eq!(settled_tool_results(&session).len(), 1);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Assistant(message) => Some(message.tool_calls.len()),
                _ => None,
            })
            .sum::<usize>(),
        1
    );
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event,
                RunEvent::ToolCallStarted(call) if call.id == "existing-call"
            ))
            .count(),
        1,
        "an already-terminal card must never receive another start"
    );
    let preview = handler
        .events
        .iter()
        .position(|event| {
            matches!(event,
                RunEvent::ToolCallStarted(call) if call.id == "fresh-preview-only"
            )
        })
        .expect("a valid preview precedes the malformed call");
    let discard = handler
        .events
        .iter()
        .position(|event| {
            matches!(event,
                RunEvent::ProviderTurnPartialOutputDiscarded(view)
                    if view.text_discarded && view.tool_request_discarded
            )
        })
        .expect("the existing failure path retires already-published previews");
    assert!(discard > preview);
    assert!(session.active_projection_snapshot()?.is_some());
    let attempts = session.provider_physical_attempt_projection()?;
    let attempts = attempts.attempts();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[1].terminal.as_ref().expect("terminal").outcome,
        ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput
    );
    drop(session);

    let mut restored = Session::load_from_store("tool-identity-fixture", "model", store)?;
    assert_eq!(settled_tool_results(&restored).len(), 1);
    let mut next = call_chunks("genuinely-new-call", true);
    next.push(ProviderChunk::Done);
    let next_agent = identity_agent(vec![next], &probe);
    let output = next_agent
        .run_with_input(
            &mut restored,
            AgentRunInput::user("continue").with_logical_run_id("identity-next-run"),
            scripted_run_options(3),
            &mut RecordingEventHandler::default(),
        )
        .await?;
    assert_eq!(output.result.final_text, "continued safely");
    assert_eq!(settled_tool_results(&restored).len(), 2);
    assert!(restored.active_projection_snapshot()?.is_some());
    Ok(())
}

#[tokio::test]
async fn recorded_tool_identity_reuse_rejects_streamed_start_and_preserves_recovery() -> Result<()>
{
    repeated_recorded_call_is_rejected_before_publication(true).await
}

#[tokio::test]
async fn recorded_tool_identity_reuse_rejects_direct_complete_and_preserves_recovery() -> Result<()>
{
    repeated_recorded_call_is_rejected_before_publication(false).await
}

#[tokio::test]
async fn recorded_tool_identity_same_id_in_distinct_sessions_remains_valid() -> Result<()> {
    let probe = Arc::new(ToolSchedulerProbe::default());
    for _ in 0..2 {
        let mut chunks = call_chunks("provider-local-id", true);
        chunks.push(ProviderChunk::Done);
        let agent = identity_agent(vec![chunks], &probe);
        let mut session = Session::new("tool-identity-fixture", "model");
        let output = agent
            .run_with_input(
                &mut session,
                AgentRunInput::user("inspect"),
                scripted_run_options(3),
                &mut RecordingEventHandler::default(),
            )
            .await?;
        assert_eq!(output.result.final_text, "continued safely");
        assert_eq!(settled_tool_results(&session).len(), 1);
    }
    assert_eq!(probe.events.lock().expect("execution events").len(), 4);
    Ok(())
}
