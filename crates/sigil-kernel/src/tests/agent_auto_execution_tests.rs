use super::*;

pub(super) fn bound_continuation_fixture(
    session: &mut Session,
    source: &ConversationTurnRef,
    prompt: &str,
) -> Result<TaskContinuationHandoffBinding> {
    let task_id = TaskId::new("task-bound-auto-fixture")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "existing task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskDirectExecutionAdmittedV1(
        TaskDirectExecutionAdmittedV1::task_request(task_id.clone(), "existing task", 1),
    ))?;
    let projected = crate::project_conversation_prompt_for_persistence(prompt);
    Ok(TaskContinuationHandoffBinding {
        task_id,
        source_turn: source.clone(),
        task_status: TaskRunStatus::Paused,
        effective_capability: AutomaticRouteCapability::DirectTask,
        policy_snapshot_hash: "sha256:bound-policy".to_owned(),
        route_contract_fingerprint: "sha256:bound-contract".to_owned(),
        decided_at_ms: 43,
        exact_guidance: SecretString::new(prompt),
        prompt_hash: projected.prompt_hash,
        exact_prompt_required: projected.exact_prompt_required,
        safe_guidance: projected.safe_prompt,
    })
}

fn ordinary_auto_input(session: &Session, prompt: &str) -> Result<AgentRunInput> {
    let input = AgentRunInput::user(prompt);
    let source = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("source message"),
        "auto-execution-run",
    )?;
    let review = test_plan_review_handoff_binding(&source, prompt);
    let task = TaskStartHandoffBinding {
        handoff_id: TaskHandoffId::new("handoff-auto-execution")?,
        task_id: TaskId::new("task-auto-execution")?,
        source_turn: source.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: prompt.to_owned(),
        policy_snapshot_hash: "sha256:auto-policy".to_owned(),
        route_contract_fingerprint: "sha256:auto-contract".to_owned(),
        requested_at_ms: 42,
        decided_at_ms: 43,
    };
    Ok(input
        .with_logical_run_id("auto-execution-run")
        .with_run_purpose(conversation_run_purpose(
            "auto-execution-run",
            source,
            TaskRoutingPolicy::Auto,
            AutomaticRouteCapability::DirectTask,
            Some(review),
            Some(task),
        )))
}

struct AutoSequenceProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
    turns: Mutex<VecDeque<Vec<ToolCall>>>,
}

#[async_trait]
impl Provider for AutoSequenceProvider {
    fn name(&self) -> &str {
        "auto-sequence"
    }
    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }
    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured.lock().expect("capture lock").push(request);
        let calls = self.turns.lock().expect("turn lock").pop_front();
        let mut chunks = vec![Ok(ProviderChunk::TextDelta(
            "visible ordinary work".to_owned(),
        ))];
        for call in calls.unwrap_or_default() {
            chunks.push(Ok(ProviderChunk::ToolCallStart {
                id: call.id.clone(),
                name: call.name.clone(),
            }));
            chunks.push(Ok(ProviderChunk::ToolCallArgsDelta {
                id: call.id.clone(),
                delta: call.args_json.clone(),
            }));
            chunks.push(Ok(ProviderChunk::ToolCallComplete(call)));
        }
        chunks.push(Ok(ProviderChunk::Done));
        Ok(Box::pin(stream::iter(chunks)))
    }
}

fn auto_call(id: &str, name: &str, args: &str) -> ToolCall {
    ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        args_json: args.to_owned(),
    }
}

fn auto_provider(
    turns: Vec<Vec<ToolCall>>,
) -> (AutoSequenceProvider, Arc<Mutex<Vec<CompletionRequest>>>) {
    let captured = Arc::new(Mutex::new(Vec::new()));
    (
        AutoSequenceProvider {
            captured: Arc::clone(&captured),
            turns: Mutex::new(turns.into()),
        },
        captured,
    )
}

#[tokio::test]
async fn ordinary_auto_executes_first_tool_and_emits_ordinary_text() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskHandoffSideEffectTool {
        executions: Arc::clone(&executions),
    }));
    let (provider, captured) = auto_provider(vec![vec![auto_call(
        "read-first",
        "handoff_side_effect",
        "{}",
    )]]);
    let agent = Agent::new(provider, registry);
    let mut session = Session::new("auto-first-tool", "model");
    let input = ordinary_auto_input(&session, "解释这个函数")?;
    let mut options = scripted_run_options(2);
    options.workspace_root = root.path().to_path_buf();
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, options, &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(captured.lock().expect("capture lock").len(), 2);
    assert!(handler.events.iter().any(
        |event| matches!(event, RunEvent::TextDelta(text) if text == "visible ordinary work")
    ));
    assert_eq!(tool_result_event_count(&handler, "read-first"), 1);
    assert!(session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) if execution.call_id == "read-first" && execution.status == ToolExecutionStatus::Completed
    )), "read execution recorded");
    assert!(!session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(decision)) if decision.route == ConversationRoute::Chat
    )), "ordinary work does not fabricate a typed routing decision");
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_can_handoff_after_completed_read_without_replaying_it() -> Result<()> {
    for tool_name in [REQUEST_PLAN_REVIEW_TOOL_NAME, START_TASK_TOOL_NAME] {
        let root = tempfile::tempdir()?;
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskHandoffSideEffectTool {
            executions: Arc::clone(&executions),
        }));
        let (provider, captured) = auto_provider(vec![
            vec![auto_call(
                "read-before-handoff",
                "handoff_side_effect",
                "{}",
            )],
            vec![auto_call(
                "positive-handoff",
                tool_name,
                if tool_name == START_TASK_TOOL_NAME {
                    r#"{}"#
                } else {
                    r#"{"reason_codes":["architectural_tradeoff"]}"#
                },
            )],
        ]);
        let agent = Agent::new(provider, registry);
        let mut session = Session::new("auto-later-handoff", "model");
        let owner = RunCancellationOwner::new();
        let input = ordinary_auto_input(&session, "inspect then choose an appropriate plan")?
            .with_cancellation(owner.handle());
        let mut options = scripted_run_options(3);
        options.workspace_root = root.path().to_path_buf();
        let output = agent
            .run_with_input(
                &mut session,
                input,
                options,
                &mut RecordingEventHandler::default(),
            )
            .await?;
        assert!(matches!(
            output.disposition,
            AgentRunDisposition::StartPlanReview(_) | AgentRunDisposition::StartDurableTask(_)
        ));
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        let requests = captured.lock().expect("capture lock");
        assert_eq!(
            requests.len(),
            2,
            "successful handoff ends the current executor"
        );
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|message| message.tool_call_id.as_deref() == Some("read-before-handoff"))
        );
        assert!(!session.entries().iter().any(|entry| matches!(entry,
            SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(decision)) if decision.route == ConversationRoute::Chat
        )));
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_mixed_handoff_and_write_executes_no_write_in_either_order() -> Result<()> {
    for handoff_first in [false, true] {
        let root = tempfile::tempdir()?;
        let executed = Arc::new(AtomicBool::new(false));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(WriteTool {
            executed: Arc::clone(&executed),
        }));
        let write = auto_call("mixed-write", "write_file", r#"{"path":"file.txt"}"#);
        let handoff = auto_call("mixed-handoff", START_TASK_TOOL_NAME, r#"{}"#);
        let calls = if handoff_first {
            vec![handoff, write]
        } else {
            vec![write, handoff]
        };
        let (provider, captured) = auto_provider(vec![calls]);
        let agent = Agent::new(provider, registry);
        let mut session = Session::new("auto-mixed-handoff", "model");
        let owner = RunCancellationOwner::new();
        let input = ordinary_auto_input(&session, "implement the change")?
            .with_cancellation(owner.handle());
        let mut options = scripted_run_options(2);
        options.workspace_root = root.path().to_path_buf();
        options.permission_config.rules = vec![crate::PermissionRule {
            tool_name: Some("write_file".to_owned()),
            subject_glob: None,
            mode: ApprovalMode::Allow,
        }];
        let output = agent
            .run_with_input(
                &mut session,
                input,
                options,
                &mut RecordingEventHandler::default(),
            )
            .await?;
        assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
        assert!(
            output
                .outcome
                .tool_errors
                .iter()
                .any(|error| error.kind == ToolErrorKind::InvalidInput)
        );
        assert!(!session.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
        )));
        assert!(!executed.load(Ordering::SeqCst));
        assert_eq!(captured.lock().expect("capture lock").len(), 2);
        assert!(!session.entries().iter().any(|entry| matches!(entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) if execution.call_id == "mixed-write" && execution.status == ToolExecutionStatus::Completed
        )));
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_first_write_still_obeys_permission_denial() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let (provider, captured) = auto_provider(vec![vec![auto_call(
        "denied-write",
        "write_file",
        r#"{"path":"file.txt"}"#,
    )]]);
    let agent = Agent::new(provider, registry);
    let mut session = Session::new("auto-denied", "model");
    let input = ordinary_auto_input(&session, "write the file")?;
    let mut options = scripted_run_options(2);
    options.workspace_root = root.path().to_path_buf();
    options.permission_config.rules = vec![crate::PermissionRule {
        tool_name: Some("write_file".to_owned()),
        subject_glob: None,
        mode: ApprovalMode::Deny,
    }];
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, options, &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(captured.lock().expect("capture lock").len(), 2);
    assert!(handler.events.iter().any(|event| matches!(event, RunEvent::ToolResult(result) if result.call_id == "denied-write" && result.is_error())));
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_cancelled_before_start_neither_calls_provider_nor_executes_tools()
-> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let (provider, captured) = auto_provider(vec![vec![auto_call(
        "cancelled-write",
        "write_file",
        r#"{"path":"file.txt"}"#,
    )]]);
    let agent = Agent::new(provider, registry);
    let mut session = Session::new("auto-cancelled", "model");
    let owner = RunCancellationOwner::new();
    let input = ordinary_auto_input(&session, "write the file")?.with_cancellation(owner.handle());
    assert!(owner.request_cancel());
    let result = agent
        .run_with_input(
            &mut session,
            input,
            scripted_run_options(2),
            &mut RecordingEventHandler::default(),
        )
        .await;
    assert!(result.is_err());
    assert!(!executed.load(Ordering::SeqCst));
    assert!(captured.lock().expect("capture lock").is_empty());
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_frozen_request_rejects_tool_schema_contract_and_source_drift() -> Result<()>
{
    for drift in [
        "tool_schema",
        "tool_access",
        "missing_interaction",
        "contract",
        "source",
    ] {
        let root = tempfile::tempdir()?;
        let mut session = Session::new("auto-frozen-drift", "model");
        let mut input = ordinary_auto_input(&session, "inspect the exact source")?;
        let mut user = ModelMessage::user("inspect the exact source");
        user.id = input
            .persisted_user_message_id
            .clone()
            .expect("source message");
        let mut options = scripted_run_options(1);
        options.workspace_root = root.path().to_path_buf();
        let mut request = session.build_pre_turn_candidate_request(
            root.path(),
            &options.memory_config,
            crate::conversation_tool_specs_for_bound_context(
                Vec::new(),
                AutomaticRouteCapability::DirectTask,
                false,
                false,
                false,
            ),
            None,
            options.reasoning_effort.clone(),
            None,
            None,
            &[
                ModelMessage::system(crate::conversation_auto_execution_contract_material()),
                user.clone(),
            ],
            RuntimeContextCandidates::default(),
            &[],
        )?;
        match drift {
            "tool_schema" => request.tools[0].input_schema = json!({"type":"object"}),
            "tool_access" => request.tools[0].access = ToolAccess::Write,
            "missing_interaction" => request
                .tools
                .retain(|spec| spec.name != REQUEST_USER_INPUT_TOOL_NAME),
            "contract" => request.messages.retain(|message| {
                message.content.as_deref()
                    != Some(crate::conversation_auto_execution_contract_material())
            }),
            "source" => {
                let source = request
                    .messages
                    .iter_mut()
                    .find(|message| message.id == user.id)
                    .expect("source turn");
                source.id = "different-source-message".to_owned();
            }
            _ => unreachable!("declared drift case"),
        }
        let frozen = FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request)?;
        session.append_user_message(user)?;
        input.persisted_user_message = None;
        input.persisted_user_message_id = None;
        input = input.with_initial_frozen_provider_request(frozen);
        let (provider, captured) = auto_provider(Vec::new());
        let agent = Agent::new(provider, ToolRegistry::new());
        let result = agent
            .run_with_input(
                &mut session,
                input,
                options,
                &mut RecordingEventHandler::default(),
            )
            .await;
        assert!(
            result.is_err(),
            "must reject {drift} before provider dispatch"
        );
        assert!(
            captured.lock().expect("capture lock").is_empty(),
            "must not dispatch {drift}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_can_handoff_after_an_approved_write_without_reexecuting_it() -> Result<()> {
    let root = tempfile::tempdir()?;
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let (provider, captured) = auto_provider(vec![
        vec![auto_call(
            "write-before-handoff",
            "write_file",
            r#"{"path":"file.txt"}"#,
        )],
        vec![auto_call(
            "after-write-handoff",
            START_TASK_TOOL_NAME,
            r#"{}"#,
        )],
    ]);
    let agent = Agent::new(provider, registry);
    let mut session = Session::new("auto-write-then-handoff", "model");
    let owner = RunCancellationOwner::new();
    let input = ordinary_auto_input(&session, "investigate and implement")?
        .with_cancellation(owner.handle());
    let mut options = scripted_run_options(3);
    options.workspace_root = root.path().to_path_buf();
    options.permission_config.rules = vec![crate::PermissionRule {
        tool_name: Some("write_file".to_owned()),
        subject_glob: None,
        mode: ApprovalMode::Allow,
    }];
    let output = agent
        .run_with_input(
            &mut session,
            input,
            options,
            &mut RecordingEventHandler::default(),
        )
        .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::StartDurableTask(_)
    ));
    assert!(executed.load(Ordering::SeqCst));
    assert_eq!(captured.lock().expect("capture lock").len(), 2);
    assert_eq!(session.entries().iter().filter(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) if execution.call_id == "write-before-handoff" && execution.status == ToolExecutionStatus::Completed
    )).count(), 1);
    assert!(!session.entries().iter().any(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(decision)) if decision.route == ConversationRoute::Chat
    )));
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_queued_followup_cannot_handoff_using_the_previous_source_binding()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let (provider, captured) = auto_provider(vec![
        Vec::new(),
        vec![auto_call("stale-handoff", START_TASK_TOOL_NAME, r#"{}"#)],
    ]);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut session = Session::new("auto-queued-source", "model");
    let owner = RunCancellationOwner::new();
    let input = ordinary_auto_input(&session, "original question")?
        .with_cancellation(owner.handle())
        .with_pending_input_provider(Arc::new(OneShotPendingInputProvider {
            remaining: AtomicUsize::new(1),
        }));
    let mut options = scripted_run_options(3);
    options.workspace_root = root.path().to_path_buf();
    let output = agent
        .run_with_input(
            &mut session,
            input,
            options,
            &mut RecordingEventHandler::default(),
        )
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    let requests = captured.lock().expect("capture lock");
    assert_eq!(requests.len(), 3);
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| message.content.as_deref() == Some("queued follow-up"))
    );
    assert!(requests[1].tools.iter().all(
        |tool| tool.name != START_TASK_TOOL_NAME && tool.name != REQUEST_PLAN_REVIEW_TOOL_NAME
    ));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
    )));
    assert_single_settled_result(
        &session,
        "stale-handoff",
        "not available for the current source turn",
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_handoff_waits_for_existing_child_obligations() -> Result<()> {
    for tool_name in [REQUEST_PLAN_REVIEW_TOOL_NAME, START_TASK_TOOL_NAME] {
        let root = tempfile::tempdir()?;
        let executions = Arc::new(AtomicUsize::new(0));
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(TaskHandoffSideEffectTool {
            executions: Arc::clone(&executions),
        }));
        let args = if tool_name == START_TASK_TOOL_NAME {
            r#"{}"#
        } else {
            r#"{"reason_codes":["architectural_tradeoff"]}"#
        };
        let (provider, captured) = auto_provider(vec![
            vec![auto_call(
                "work-before-child-wait",
                "handoff_side_effect",
                "{}",
            )],
            vec![auto_call("blocked-handoff", tool_name, args)],
            vec![auto_call("settled-handoff", tool_name, args)],
        ]);
        let agent = Agent::new(provider, registry);
        let mut session = Session::new("auto-handoff-child-obligation", "model");
        let owner = RunCancellationOwner::new();
        let input = ordinary_auto_input(&session, "finish existing child work before planning")?
            .with_cancellation(owner.handle());
        let mut options = scripted_run_options(4);
        options.workspace_root = root.path().to_path_buf();
        let mut delegate = SequencedFinalAnswerBlockerDelegate {
            blockers: VecDeque::from([Some("pending owned child must be joined".to_owned()), None]),
            fallback: None,
        };
        let output = agent
            .run_with_approval_input_and_agent_delegate(
                &mut session,
                input,
                options,
                &mut RecordingEventHandler::default(),
                &mut AutoApproveHandler,
                &mut delegate,
            )
            .await?;
        assert!(matches!(
            output.disposition,
            AgentRunDisposition::StartPlanReview(_) | AgentRunDisposition::StartDurableTask(_)
        ));
        assert_eq!(
            captured.lock().expect("capture lock").len(),
            3,
            "the first handoff must wait for child obligations"
        );
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_single_settled_result(
            &session,
            "blocked-handoff",
            "pending owned child must be joined",
        );
        assert_eq!(
            session
                .entries()
                .iter()
                .filter(|entry| matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(_))
                ))
                .count(),
            1
        );
    }
    Ok(())
}

struct RecoveryProbeTool {
    initial_error: ToolErrorKind,
    calls: AtomicUsize,
}

#[async_trait]
impl Tool for RecoveryProbeTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "recovery_probe".to_owned(),
            description: "Probe the workspace and return its typed recovery state".to_owned(),
            input_schema: json!({"type":"object","additionalProperties":false}),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Ok(ToolResult::error(
                call_id,
                "recovery_probe",
                self.initial_error,
                "workspace observation needs recovery",
            ))
        } else {
            Ok(ToolResult::ok(
                call_id,
                "recovery_probe",
                "workspace observation recovered",
                ToolResultMeta::default(),
            ))
        }
    }
}

#[tokio::test]
async fn ordinary_auto_handoff_cannot_bypass_interrupted_or_active_recovery_tool() -> Result<()> {
    for error_kind in [ToolErrorKind::Interrupted, ToolErrorKind::WorkspaceConflict] {
        let root = tempfile::tempdir()?;
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(RecoveryProbeTool {
            initial_error: error_kind,
            calls: AtomicUsize::new(0),
        }));
        let (provider, captured) = auto_provider(vec![
            vec![auto_call("unresolved-probe", "recovery_probe", "{}")],
            vec![auto_call(
                "blocked-recovery-handoff",
                START_TASK_TOOL_NAME,
                r#"{}"#,
            )],
        ]);
        let agent = Agent::new(provider, registry);
        let mut session = Session::new("auto-active-recovery", "model");
        let owner = RunCancellationOwner::new();
        let input = ordinary_auto_input(&session, "investigate before planning")?
            .with_cancellation(owner.handle());
        let mut options = scripted_run_options(3);
        options.workspace_root = root.path().to_path_buf();
        let output = agent
            .run_with_input(
                &mut session,
                input,
                options,
                &mut RecordingEventHandler::default(),
            )
            .await?;
        assert!(!matches!(
            output.disposition,
            AgentRunDisposition::StartPlanReview(_) | AgentRunDisposition::StartDurableTask(_)
        ));
        assert_eq!(captured.lock().expect("capture lock").len(), 3);
        assert!(
            output
                .outcome
                .tool_errors
                .iter()
                .any(|error| error.kind == error_kind)
        );
        assert_single_settled_result(
            &session,
            "blocked-recovery-handoff",
            "active recovery blocker",
        );
        assert!(!session.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
        )));
    }
    Ok(())
}

#[tokio::test]
async fn ordinary_auto_handoff_resumes_after_a_typed_tool_receipt_resolves_recovery() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RecoveryProbeTool {
        initial_error: ToolErrorKind::WorkspaceConflict,
        calls: AtomicUsize::new(0),
    }));
    let (provider, captured) = auto_provider(vec![
        vec![auto_call("conflicted-probe", "recovery_probe", "{}")],
        vec![auto_call(
            "blocked-conflict-handoff",
            START_TASK_TOOL_NAME,
            r#"{}"#,
        )],
        vec![auto_call("recovered-probe", "recovery_probe", "{}")],
        vec![auto_call(
            "recovered-handoff",
            START_TASK_TOOL_NAME,
            r#"{}"#,
        )],
    ]);
    let agent = Agent::new(provider, registry);
    let mut session = Session::new("auto-resolved-recovery", "model");
    let owner = RunCancellationOwner::new();
    let input = ordinary_auto_input(&session, "recover the observation before planning")?
        .with_cancellation(owner.handle());
    let mut options = scripted_run_options(4);
    options.workspace_root = root.path().to_path_buf();
    let output = agent
        .run_with_input(
            &mut session,
            input,
            options,
            &mut RecordingEventHandler::default(),
        )
        .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::StartDurableTask(_)
    ));
    assert_eq!(captured.lock().expect("capture lock").len(), 4);
    let error = output
        .outcome
        .tool_errors
        .iter()
        .find(|error| error.kind == ToolErrorKind::WorkspaceConflict)
        .expect("historical conflict remains auditable");
    assert_eq!(error.details.get("active"), Some(&json!(false)));
    assert_eq!(
        error.details.get("resolved_by_call_id"),
        Some(&json!("recovered-probe"))
    );
    assert_single_settled_result(
        &session,
        "blocked-conflict-handoff",
        "active recovery blocker",
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            ))
            .count(),
        1
    );
    Ok(())
}
