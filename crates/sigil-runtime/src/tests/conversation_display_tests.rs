use std::collections::HashSet;

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sigil_kernel::{
    AgentRole, ApprovalMode, AssistantMessageKind, CheckpointRestoreConflict,
    CheckpointRestoreConflictReason, ContextBodyRef, ContextInclusionReason, ContextItem,
    ContextSensitivity, ContextSource, ContextTrustLevel, ControlEntry, ConversationForked,
    ConversationInputKind, ConversationInputPromotedEntry, ConversationInputQueueId,
    ConversationInputQueuedEntry, ConversationInputTarget, ConversationRunFinalizedEntryV1,
    ConversationRunStartedEntryV1, ConversationRunTerminalStatusV1, DurableEventType, EventClass,
    JsonlSessionStore, MemoryConfig, MessageRole, ModelMessage, PermissionRisk,
    RuntimeContextCandidates, SecretRedactor, Session, SessionLogEntry, SessionRef,
    SessionStreamRecord, SkillLoadEntry, SkillSource, StoredEvent, TaskId, TaskIsolationMode,
    TaskPlanEntry, TaskPlanStatus, TaskRunCancellationScopeBoundEntry, TaskRunEntry, TaskRunStatus,
    TaskRunTargetSelectedEntry, TaskStepEntry, TaskStepId, TaskStepMode, TaskStepSpec,
    TaskStepStatus, ToolAccess, ToolApprovalAuditAction, ToolApprovalDecisionReceiptV2,
    ToolApprovalEntry, ToolApprovalTerminalStatusV2, ToolApprovalUserDecision,
    ToolArtifactSensitivity, ToolArtifactStore, ToolCall, ToolOperation, ToolResult,
    ToolResultMeta, ToolResultRecordedV3, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
};

use crate::conversation_display::{
    ConversationDisplayAssistantPhaseV1, ConversationDisplayCheckpointConflictReasonV1,
    ConversationDisplayContentV1, ConversationDisplayItemKindV1, ConversationDisplayMessageRoleV1,
    ConversationDisplayProjectionError, ConversationDisplayStatusV1,
    ConversationLiveProvisionalSlotV1, MAX_CONVERSATION_DISPLAY_CONTENT_BYTES,
    MAX_CONVERSATION_DISPLAY_PAGE_BYTES, MAX_CONVERSATION_DISPLAY_PAGE_SIZE,
    MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS, MAX_CONVERSATION_TASK_CONTROL_ITEMS,
    MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES, PlanReviewCompatibilityStatusV1,
    conversation_display_page, conversation_display_page_from_records,
    conversation_live_provisional_id, plan_review_compatibility_from_entries,
    public_plan_review_from_entries,
};

fn durable_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store.clone());
    Ok((temp, store, session))
}

fn internal_context_fixture() -> RuntimeContextCandidates {
    let body = "provider-only context snapshot body";
    let mut candidates = RuntimeContextCandidates::new();
    candidates.items.push(ContextItem {
        id: "context-display-fixture".to_owned(),
        source: ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: Some("context-display-snapshot".to_owned()),
        token_cost: sigil_kernel::estimate_context_token_cost(body),
        score: Some(100.0),
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: ContextBodyRef::inline(body),
    });
    candidates
        .snippets
        .insert("context-display-fixture".to_owned(), body.to_owned());
    candidates
}

#[test]
fn conversation_display_hides_provider_visible_context_v2_snapshots() -> Result<()> {
    let (temp, store, mut session) = durable_session()?;
    session.append_user_message(ModelMessage::user("inspect the display contract"))?;
    session.build_request_with_transient_messages_and_context(
        temp.path(),
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        None,
        None,
        &[],
        internal_context_fixture(),
    )?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;

    let page = conversation_display_page(store.path(), session.session_scope_id(), None, 20, None)?;
    assert_eq!(page.items.len(), 2);
    assert!(!format!("{page:?}").contains("provider-only context snapshot body"));
    Ok(())
}

fn approval_entry(
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
) -> ToolApprovalEntry {
    let call_id = "approval-call".to_owned();
    let plan_hash = sigil_kernel::stable_event_hash("approval-plan");
    let decision_receipt = (action == ToolApprovalAuditAction::DecisionAccepted).then(|| {
        ToolApprovalDecisionReceiptV2 {
            approval_request_id: "approval-display".to_owned(),
            decision: user_decision.expect("accepted approval fixture has a decision"),
            accepted_at_ms: 1_000,
        }
    });
    let terminal_status =
        (action == ToolApprovalAuditAction::Resolved).then_some(match user_decision {
            Some(ToolApprovalUserDecision::Approved) => ToolApprovalTerminalStatusV2::Approved,
            Some(ToolApprovalUserDecision::ApprovedForSession) => {
                ToolApprovalTerminalStatusV2::ApprovedForSession
            }
            Some(ToolApprovalUserDecision::Denied) | None => ToolApprovalTerminalStatusV2::Denied,
        });
    ToolApprovalEntry {
        schema_version: sigil_kernel::TOOL_APPROVAL_AUDIT_SCHEMA_VERSION,
        identity: sigil_kernel::ApprovalRequestIdentityV2 {
            session_id: "session-display".to_owned(),
            run_id: "run-display".to_owned(),
            call_id: call_id.clone(),
            approval_request_id: "approval-display".to_owned(),
            plan_hash: plan_hash.clone(),
            policy_version: "policy-display".to_owned(),
            execution_binding_hash: plan_hash.clone(),
            expires_at_ms: u64::MAX,
        },
        plan_hash,
        action,
        call_id,
        tool_name: "bash".to_owned(),
        access: ToolAccess::Execute,
        network_effect: None,
        local_policy_decision: ApprovalMode::Ask,
        network_policy_decision: ApprovalMode::Allow,
        source_policy_decision: ApprovalMode::Allow,
        operation: ToolOperation::ExecuteUnknownCommand,
        risk: PermissionRisk::Medium,
        subjects: Vec::new(),
        subject_zones: Vec::new(),
        policy_decision: ApprovalMode::Ask,
        external_directory_required: false,
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        decision_reasons: Vec::new(),
        user_decision,
        reason: None,
        preview_hash: None,
        decision_receipt,
        terminal_status,
    }
}

#[test]
fn canonical_projection_has_stable_ids_orders_and_run_binding() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-1", 10)?)?;

    session.append_user_message(ModelMessage::user("inspect this"))?;
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{"path":"secret"}"#.to_owned(),
    };
    let final_message = ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        vec![tool_call],
        AssistantMessageKind::FinalAnswer,
    );
    let final_message_id = final_message.id.clone();
    session.append_assistant_message(final_message)?;
    let artifact_store = session
        .tool_artifact_store()
        .expect("durable session exposes its artifact store");
    let (recorded, _) = ToolResultRecordedV3::capture(
        &ToolResult::ok(
            "call-1",
            "read_file",
            "file output",
            ToolResultMeta::default(),
        ),
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    session.append(SessionLogEntry::ToolResultV3(recorded))?;
    recorder.append_finalized(&ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some(final_message_id.clone()),
        Some("complete"),
        20,
        &SecretRedactor::empty(),
    )?)?;

    let first = conversation_display_page(store.path(), &scope, None, 20, None)?;
    let second = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert_eq!(first, second);
    assert_eq!(first.items.len(), 5);
    assert!(
        first
            .items
            .windows(2)
            .all(|items| items[0].display_order < items[1].display_order)
    );
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.display_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        first.items.len()
    );
    assert!(
        first
            .items
            .iter()
            .all(|item| item.run_id.as_deref() == Some("run-1"))
    );
    assert!(first.items.iter().all(|item| item.run_sequence.is_none()));
    assert!(
        first
            .items
            .iter()
            .all(|item| !item.source_event_id.is_empty())
    );
    assert_eq!(
        first
            .items
            .iter()
            .filter(|item| item.kind == ConversationDisplayItemKindV1::Terminal)
            .count(),
        1
    );
    assert_eq!(
        first
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.content,
                    ConversationDisplayContentV1::Message {
                        assistant_phase: Some(ConversationDisplayAssistantPhaseV1::FinalAnswer),
                        ..
                    }
                )
            })
            .count(),
        1,
        "terminal evidence must not duplicate the final assistant answer"
    );
    let expected_user = conversation_live_provisional_id(
        &scope,
        "run-1",
        &ConversationLiveProvisionalSlotV1::User,
    )?;
    let expected_final = conversation_live_provisional_id(
        &scope,
        "run-1",
        &ConversationLiveProvisionalSlotV1::AssistantMessage {
            message_id: final_message_id,
        },
    )?;
    let expected_tool = conversation_live_provisional_id(
        &scope,
        "run-1",
        &ConversationLiveProvisionalSlotV1::Tool {
            call_id: "call-1".to_owned(),
        },
    )?;
    let expected_terminal = conversation_live_provisional_id(
        &scope,
        "run-1",
        &ConversationLiveProvisionalSlotV1::Terminal,
    )?;
    let user = first
        .items
        .iter()
        .find(|item| item.kind == ConversationDisplayItemKindV1::UserMessage)
        .expect("durable user item");
    assert_eq!(user.reconciles.as_deref(), Some(&[expected_user][..]));
    let final_answer = first
        .items
        .iter()
        .find(|item| {
            matches!(
                item.content,
                ConversationDisplayContentV1::Message {
                    assistant_phase: Some(ConversationDisplayAssistantPhaseV1::FinalAnswer),
                    ..
                }
            )
        })
        .expect("durable final answer");
    assert_eq!(
        final_answer.reconciles.as_deref(),
        Some(&[expected_final][..])
    );
    let tools = first
        .items
        .iter()
        .filter(|item| item.kind == ConversationDisplayItemKindV1::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tools.len(), 2);
    assert_eq!(
        tools[0].reconciles.as_deref(),
        Some(&[expected_tool.clone()][..])
    );
    assert_eq!(
        tools[1].reconciles.as_deref(),
        Some(&[tools[0].display_id.clone(), expected_tool][..]),
        "completed tool evidence must replace both the earlier durable request and live slot"
    );
    let terminal = first
        .items
        .iter()
        .find(|item| item.kind == ConversationDisplayItemKindV1::Terminal)
        .expect("durable terminal evidence");
    assert_eq!(
        terminal.reconciles.as_deref(),
        Some(&[expected_terminal][..])
    );
    assert_eq!(
        first
            .terminal_frontier
            .as_ref()
            .map(|frontier| (frontier.run_id.as_str(), frontier.status,)),
        Some(("run-1", ConversationDisplayStatusV1::Succeeded))
    );
    Ok(())
}

#[test]
fn only_the_initial_user_message_reconciles_the_live_run_started_slot() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new("run-queued-input", 10)?)?;
    session.append_user_message(ModelMessage::user("initial prompt"))?;
    session.append_user_message(ModelMessage::user("queued safe-point follow-up"))?;

    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    let users = page
        .items
        .iter()
        .filter(|item| item.kind == ConversationDisplayItemKindV1::UserMessage)
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 2);
    assert_eq!(
        users[0].reconciles.as_deref(),
        Some(
            &[conversation_live_provisional_id(
                &scope,
                "run-queued-input",
                &ConversationLiveProvisionalSlotV1::User,
            )?][..]
        )
    );
    assert_eq!(
        users[1].reconciles, None,
        "a later durable user message has no matching RunStarted live slot"
    );
    Ok(())
}

#[test]
fn user_selected_skill_is_projected_on_its_durable_prompt() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_control(ControlEntry::SkillLoaded(SkillLoadEntry {
        skill_id: "compat-skill-123".to_owned(),
        display_name: Some("唐代城市研究".to_owned()),
        sha256: "sha256:skill".to_owned(),
        source: SkillSource::Workspace,
        entrypoint: ".agents/skills/changan/SKILL.md".into(),
        run_id: Some("run-skill".to_owned()),
        call_id: None,
        byte_count: 128,
        line_count: 7,
        loaded_at_ms: 9,
    }))?;
    session
        .conversation_run_lifecycle_recorder()?
        .append_started(&ConversationRunStartedEntryV1::new("run-skill", 10)?)?;
    session.append_user_message(ModelMessage::user("研究唐代长安城"))?;

    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    let user = page
        .items
        .iter()
        .find(|item| item.kind == ConversationDisplayItemKindV1::UserMessage)
        .expect("durable user message");
    assert!(matches!(
        &user.content,
        ConversationDisplayContentV1::Message {
            skill: Some(skill),
            ..
        } if skill.id == "compat-skill-123" && skill.name == "唐代城市研究"
    ));
    Ok(())
}

#[test]
fn promoted_input_is_the_single_durable_user_display_event() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let queue_id = ConversationInputQueueId::new("queue-display-1")?;
    let prompt = project_conversation_prompt_for_persistence("inspect the queue contract");
    session.append_control(ControlEntry::ConversationInputQueued(
        ConversationInputQueuedEntry {
            queue_id: queue_id.clone(),
            target: ConversationInputTarget::MainThread,
            kind: ConversationInputKind::Chat,
            prompt_hash: prompt.prompt_hash.clone(),
            prompt: prompt.safe_prompt.clone(),
            reasoning_effort: None,
            created_at_ms: Some(1),
        },
    ))?;
    let queue = session
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("queued input should have a durable projection");
    let revision = queue
        .revision
        .expect("queued input should establish a queue revision");
    let mut durable_user_message = ModelMessage::user(prompt.safe_prompt);
    durable_user_message.id = "queued-display-message-1".to_owned();
    let promotion = ConversationInputPromotedEntry {
        queue_id,
        expected_queue_revision: revision,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: prompt.exact_prompt_required,
        durable_user_message,
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "queued-display-run-1".to_owned(),
        promoted_at_ms: 2,
    };
    let promoted = store.append_conversation_input_promoted(promotion)?;

    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    let users = page
        .items
        .iter()
        .filter(|item| item.kind == ConversationDisplayItemKindV1::UserMessage)
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].source_event_id, promoted.event_id);
    assert_eq!(users[0].run_id.as_deref(), Some("queued-display-run-1"));
    assert_eq!(
        users[0].reconciles.as_deref(),
        Some(
            &[conversation_live_provisional_id(
                &scope,
                "queued-display-run-1",
                &ConversationLiveProvisionalSlotV1::User,
            )?][..]
        )
    );
    assert!(matches!(
        users[0].content,
        ConversationDisplayContentV1::Message {
            role: ConversationDisplayMessageRoleV1::User,
            text: Some(ref text),
            ..
        } if text == "inspect the queue contract"
    ));

    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind()
                    == Some(DurableEventType::ConversationInputPromoted)
            })
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0,
        "promotion must not require a second durable user-message event"
    );
    Ok(())
}

#[test]
fn terminal_must_match_the_unique_durable_final_for_its_active_run() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-1", 10)?)?;
    let final_message = ModelMessage::assistant_with_kind(
        Some("durable answer".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    );
    session.append_assistant_message(final_message)?;
    recorder.append_finalized(&ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("another-message".to_owned()),
        Some("complete"),
        20,
        &SecretRedactor::empty(),
    )?)?;

    assert!(
        conversation_display_page(store.path(), &scope, None, 20, None)
            .expect_err("succeeded terminal must bind the active run's durable final")
            .to_string()
            .contains("does not match")
    );

    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-2", 30)?)?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("first final".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("second final".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    assert!(
        conversation_display_page(store.path(), &scope, None, 20, None)
            .expect_err("one run cannot project two durable final assistants")
            .to_string()
            .contains("more than one")
    );
    Ok(())
}

#[test]
fn approval_phases_form_one_reconciliation_chain() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let recorder = session.conversation_run_lifecycle_recorder()?;
    recorder.append_started(&ConversationRunStartedEntryV1::new("run-approval", 10)?)?;
    session.append_control(ControlEntry::ToolApproval(approval_entry(
        ToolApprovalAuditAction::Requested,
        None,
    )))?;
    session.append_control(ControlEntry::ToolApproval(approval_entry(
        ToolApprovalAuditAction::DecisionAccepted,
        Some(ToolApprovalUserDecision::Approved),
    )))?;
    session.append_control(ControlEntry::ToolApproval(approval_entry(
        ToolApprovalAuditAction::Resolved,
        Some(ToolApprovalUserDecision::Approved),
    )))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    let approvals = page
        .items
        .iter()
        .filter(|item| item.kind == ConversationDisplayItemKindV1::Approval)
        .collect::<Vec<_>>();
    assert_eq!(approvals.len(), 3);
    let live_id = conversation_live_provisional_id(
        &scope,
        "run-approval",
        &ConversationLiveProvisionalSlotV1::Approval {
            call_id: "approval-call".to_owned(),
        },
    )?;
    assert_eq!(
        approvals[0].reconciles.as_deref(),
        Some(&[live_id.clone()][..])
    );
    assert_eq!(
        approvals[1].reconciles.as_deref(),
        Some(&[approvals[0].display_id.clone(), live_id.clone()][..])
    );
    assert_eq!(
        approvals[2].reconciles.as_deref(),
        Some(&[approvals[1].display_id.clone(), live_id.clone()][..])
    );
    assert!(
        approvals[2]
            .reconciles
            .as_ref()
            .expect("resolved approval reconciliation")
            .iter()
            .all(|identity| !identity.contains(&scope) && !identity.contains("approval-call"))
    );
    Ok(())
}

#[test]
fn unbound_messages_do_not_synthesize_terminal_items() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_user_message(ModelMessage::user("unbound user"))?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("unbound answer".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    store.append_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "completed",
            "final_message_id": null,
            "tool_calls": 0,
            "error": null
        }),
    )?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|item| item.run_id.is_none()));
    assert!(
        page.items
            .iter()
            .all(|item| item.kind != ConversationDisplayItemKindV1::Terminal)
    );
    assert!(page.terminal_frontier.is_none());
    Ok(())
}

#[test]
fn conversation_fork_receipt_projects_as_a_safe_timeline_notice() -> Result<()> {
    let (_temp, store, session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let fork = ConversationForked {
        fork_id: "fork-1".to_owned(),
        parent_session_ref: sigil_kernel::SessionRef::new_relative("parent.jsonl")?,
        source_session_id: "source-scope".to_owned(),
        source_turn_index: 3,
        source_boundary_event_id: "boundary-1".to_owned(),
        source_boundary_stream_sequence: 7,
        source_turn_digest: "turn-digest".to_owned(),
        source_checkpoint_id: None,
        source_checkpoint_digest: None,
        destination_session_id: scope.clone(),
        copied_message_count: 6,
        copied_external_provenance_count: 0,
    };
    store.append_event(
        DurableEventType::ConversationForked,
        EventClass::Critical,
        serde_json::to_value(fork)?,
    )?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, ConversationDisplayItemKindV1::Notice);
    assert_eq!(page.items[0].status, ConversationDisplayStatusV1::Completed);
    assert!(matches!(
        &page.items[0].content,
        ConversationDisplayContentV1::Notice { text, .. }
            if text.contains("turn 3") && text.contains("workspace files were not changed")
    ));
    Ok(())
}

#[test]
fn production_display_reconciles_declared_artifact_state_with_physical_availability() -> Result<()>
{
    let (_temp, store, session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let artifact_store = ToolArtifactStore::for_session_store(&store);
    let (recorded, _) = ToolResultRecordedV3::capture(
        &ToolResult::ok(
            "call-artifact-display",
            "shell",
            "complete artifact body",
            ToolResultMeta::default(),
        ),
        Some(&artifact_store),
        ToolArtifactSensitivity::Ordinary,
    )?;
    let artifact_ref = recorded
        .artifact
        .descriptor()
        .expect("published artifact")
        .artifact_ref
        .clone();
    store.append(&SessionLogEntry::ToolResultV3(recorded))?;

    let available = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert!(matches!(
        &available.items[0].content,
        ConversationDisplayContentV1::Tool {
            artifact_availability: Some(value),
            ..
        } if value == "available"
    ));

    std::fs::remove_file(
        artifact_store
            .root()
            .join("refs")
            .join(format!("{}.json", artifact_ref.artifact_id)),
    )?;
    let missing = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert!(matches!(
        &missing.items[0].content,
        ConversationDisplayContentV1::Tool {
            artifact_availability: Some(value),
            ..
        } if value == "missing"
    ));
    Ok(())
}

#[test]
fn cursor_pins_a_fixed_frontier_while_new_history_is_appended() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    for index in 0..5 {
        session.append_user_message(ModelMessage::user(format!("message-{index}")))?;
    }

    let first = conversation_display_page(store.path(), &scope, None, 2, None)?;
    assert_eq!(first.items.len(), 2);
    assert!(first.has_more);
    let cursor = first.next_cursor.clone().expect("older page cursor");
    let decoded_cursor = String::from_utf8(URL_SAFE_NO_PAD.decode(&cursor)?)?;
    assert!(!decoded_cursor.contains(&scope));
    for record in JsonlSessionStore::read_event_records(store.path())? {
        assert!(!decoded_cursor.contains(record.event_id()));
        assert!(!decoded_cursor.contains(record.record_checksum()));
    }
    let mut forged_payload: serde_json::Value = serde_json::from_str(&decoded_cursor)?;
    forged_payload["before_order"]["subindex"] = json!(99);
    let forged_cursor = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged_payload)?);
    assert!(
        conversation_display_page(store.path(), &scope, Some(&forged_cursor), 2, None)
            .expect_err("a re-encoded cursor boundary must not be forgeable")
            .to_string()
            .contains("frontier")
    );
    let first_ids = first
        .items
        .iter()
        .map(|item| item.display_id.clone())
        .collect::<HashSet<_>>();

    session.append_user_message(ModelMessage::user("new-after-frontier"))?;
    let second = conversation_display_page(store.path(), &scope, Some(&cursor), 2, None)?;
    assert_eq!(
        second.through_session_stream_sequence,
        first.through_session_stream_sequence
    );
    assert_eq!(second.total_items, 5);
    assert!(
        second
            .items
            .iter()
            .all(|item| !first_ids.contains(&item.display_id))
    );
    assert!(second.items.iter().all(|item| {
        !matches!(
            &item.content,
            ConversationDisplayContentV1::Message { text: Some(text), .. }
                if text == "new-after-frontier"
        )
    }));

    assert!(matches!(
        conversation_display_page(store.path(), "another-scope", Some(&cursor), 2, None),
        Err(ConversationDisplayProjectionError::InvalidCursor { .. })
    ));
    let mut tampered = cursor;
    tampered.push('x');
    assert!(matches!(
        conversation_display_page(store.path(), &scope, Some(&tampered), 2, None),
        Err(ConversationDisplayProjectionError::InvalidCursor { .. })
    ));
    assert!(matches!(
        conversation_display_page(store.path(), &scope, Some("e30"), 2, None),
        Err(ConversationDisplayProjectionError::InvalidCursor { .. })
    ));

    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(matches!(
        conversation_display_page_from_records(
            &records[..2],
            &scope,
            Some(&first.next_cursor.expect("cursor")),
            2,
            None,
        ),
        Err(ConversationDisplayProjectionError::StaleCursor { .. })
    ));
    Ok(())
}

#[test]
fn projection_is_secret_safe_and_bounded_by_item_page_and_limit() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let large_content = "x".repeat(70_000);
    for _ in 0..12 {
        session.append_user_message(ModelMessage::user(large_content.clone()))?;
    }
    session.append_user_message(ModelMessage::user("token=sk-test-secret"))?;

    let page = conversation_display_page(store.path(), &scope, None, 12, None)?;
    assert!(
        page.has_more,
        "page byte budget should preserve an older cursor"
    );
    assert!(serde_json::to_vec(&page.items)?.len() <= MAX_CONVERSATION_DISPLAY_PAGE_BYTES);
    for item in &page.items {
        let ConversationDisplayContentV1::Message {
            text: Some(text),
            truncated,
            original_content_bytes,
            ..
        } = &item.content
        else {
            panic!("expected message content");
        };
        assert!(text.len() <= MAX_CONVERSATION_DISPLAY_CONTENT_BYTES);
        if *original_content_bytes == large_content.len() {
            assert!(*truncated);
        }
        assert!(!text.contains("sk-"));
    }
    assert!(conversation_display_page(store.path(), &scope, None, 0, None).is_err());
    assert!(
        conversation_display_page(
            store.path(),
            &scope,
            None,
            MAX_CONVERSATION_DISPLAY_PAGE_SIZE + 1,
            None,
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn reasoning_is_typed_and_empty_messages_do_not_create_placeholders() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("reasoning details".to_owned()),
        Vec::new(),
        AssistantMessageKind::ReasoningTrace,
    ))?;
    session.append_user_message(ModelMessage::new(MessageRole::User, None))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].kind, ConversationDisplayItemKindV1::Reasoning);
    assert!(matches!(
        &page.items[0].content,
        ConversationDisplayContentV1::Reasoning { text, .. } if text == "reasoning details"
    ));
    Ok(())
}

#[test]
fn intent_state_checkpoint_conflict_projects_as_typed_display_reason() -> Result<()> {
    let conflict = CheckpointRestoreConflict {
        checkpoint_id: "checkpoint-1".to_owned(),
        checkpoint_digest: "digest-1".to_owned(),
        path: Some("src/lib.rs".into()),
        reason: CheckpointRestoreConflictReason::IntentStateConflict,
        expected_current_hash: None,
        actual_current_hash: None,
    };
    let record = SessionStreamRecord::Stored(StoredEvent::new(
        DurableEventType::CheckpointRestoreConflict,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        serde_json::to_value(conflict)?,
    )?);

    let page = conversation_display_page_from_records(&[record], "scope-1", None, 10, None)?;
    assert_eq!(page.items.len(), 1);
    assert!(matches!(
        page.items[0].content,
        ConversationDisplayContentV1::Checkpoint {
            conflict_reason: Some(
                ConversationDisplayCheckpointConflictReasonV1::IntentStateConflict
            ),
            ..
        }
    ));
    Ok(())
}

#[test]
fn unknown_critical_lifecycle_and_checksum_tampering_fail_closed() -> Result<()> {
    let unknown = SessionStreamRecord::Stored(StoredEvent::new_raw(
        "future_critical_event",
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({"future": true}),
    )?);
    assert!(
        conversation_display_page_from_records(&[unknown], "scope-1", None, 10, None)
            .expect_err("unknown critical event must fail")
            .to_string()
            .contains("unknown critical")
    );

    let future_lifecycle = SessionStreamRecord::Stored(StoredEvent::new(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({"record": "conversation_run_started_v2"}),
    )?);
    assert!(
        conversation_display_page_from_records(&[future_lifecycle], "scope-1", None, 10, None)
            .expect_err("future critical lifecycle tag must fail")
            .to_string()
            .contains("unknown critical run lifecycle")
    );

    let mut tampered = StoredEvent::new(
        DurableEventType::UserMessageRecorded,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({"session_log_entry": SessionLogEntry::User(ModelMessage::user("hello"))}),
    )?;
    tampered.record_checksum.push('0');
    assert!(
        conversation_display_page_from_records(
            &[SessionStreamRecord::Stored(tampered)],
            "scope-1",
            None,
            10,
            None,
        )
        .expect_err("tampered checksum must fail")
        .to_string()
        .contains("checksum")
    );
    Ok(())
}

#[test]
fn role_mismatch_and_overlapping_runs_fail_closed() -> Result<()> {
    let mismatched = StoredEvent::new(
        DurableEventType::UserMessageRecorded,
        EventClass::Critical,
        "event-1".to_owned(),
        "scope-1".to_owned(),
        1,
        json!({
            "session_log_entry": SessionLogEntry::User(ModelMessage::assistant(
                Some("wrong role".to_owned()),
                Vec::new(),
            ))
        }),
    )?;
    assert!(
        conversation_display_page_from_records(
            &[SessionStreamRecord::Stored(mismatched)],
            "scope-1",
            None,
            10,
            None,
        )
        .expect_err("role mismatch must fail")
        .to_string()
        .contains("non-user role")
    );

    let start_one = ConversationRunStartedEntryV1::new("run-1", 1)?;
    let start_two = ConversationRunStartedEntryV1::new("run-2", 2)?;
    let records = vec![
        SessionStreamRecord::Stored(StoredEvent::new(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            "event-1".to_owned(),
            "scope-1".to_owned(),
            1,
            serde_json::to_value(
                sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(start_one),
            )?,
        )?),
        SessionStreamRecord::Stored(StoredEvent::new(
            DurableEventType::RunStatusChanged,
            EventClass::Critical,
            "event-2".to_owned(),
            "scope-1".to_owned(),
            2,
            serde_json::to_value(
                sigil_kernel::ConversationRunLifecycleRecordV1::ConversationRunStartedV1(start_two),
            )?,
        )?),
    ];
    assert!(
        conversation_display_page_from_records(&records, "scope-1", None, 10, None)
            .expect_err("overlapping runs must fail")
            .to_string()
            .contains("overlapping")
    );
    Ok(())
}

#[test]
fn message_content_role_remains_provider_neutral() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    session.append_user_message(ModelMessage::user("hello"))?;
    let page = conversation_display_page(store.path(), &scope, None, 1, None)?;
    assert!(matches!(
        page.items[0].content,
        ConversationDisplayContentV1::Message {
            role: ConversationDisplayMessageRoleV1::User,
            ..
        }
    ));
    Ok(())
}

#[test]
fn durable_task_control_restores_paused_task_without_private_objective() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let task_id = TaskId::new("task-restart-control")?;
    let step_id = TaskStepId::new("inspect-code")?;
    let secret_objective = "private objective with AK-DO-NOT-EXPOSE and /private/worktree";

    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: secret_objective.to_owned(),
        title: None,

        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "Inspect the durable state".to_owned(),
            display_name: None,
            detail: Some("private planner detail".to_owned()),
            role: AgentRole::SubagentRead,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id,
        role: AgentRole::SubagentRead,
        status: TaskStepStatus::Interrupted,
        title: Some("private runtime title".to_owned()),
        summary: Some("private transcript summary".to_owned()),
        reason: Some("private interruption reason".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: secret_objective.to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: Some("private pause reason".to_owned()),
    }))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    let task = page
        .task_control
        .as_ref()
        .expect("paused task control should survive restart");
    assert_eq!(task.task_id, task_id.as_str());
    assert_eq!(task.status, "paused");
    assert_eq!(task.phase, sigil_kernel::PublicTaskPhase::Execution);
    assert_eq!(task.plan_version, Some(1));
    assert_eq!(task.plan_status.as_deref(), Some("accepted"));
    assert_eq!(task.steps.len(), 1);
    assert_eq!(task.steps[0].status.as_deref(), Some("interrupted"));
    assert!(!task.steps_truncated);
    assert!(!task.lanes_truncated);
    assert!(task.can_continue);

    let serialized = serde_json::to_string(&page)?;
    assert!(!serialized.contains(secret_objective));
    assert!(!serialized.contains("private planner detail"));
    assert!(!serialized.contains("private runtime title"));
    assert!(!serialized.contains("private transcript summary"));
    assert!(!serialized.contains("private interruption reason"));
    assert!(!serialized.contains("parent.jsonl"));

    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: secret_objective.to_owned(),
        title: None,

        status: TaskRunStatus::Completed,
        reason: None,
    }))?;
    let completed = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert!(completed.task_control.is_none());
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: TaskId::new("task-restart-control")?,
        plan_version: 1,
        step_id: TaskStepId::new("inspect-code")?,
        role: AgentRole::SubagentRead,
        status: TaskStepStatus::Running,
        title: None,
        summary: None,
        reason: None,
    }))?;
    let late_step = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert!(late_step.task_control.is_none());
    Ok(())
}

#[test]
fn unrelated_chat_makes_paused_task_historical_despite_late_task_events() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let task_id = TaskId::new("task-historical-after-chat")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "old durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: Vec::new(),
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "old durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for follow-up".to_owned()),
    }))?;
    assert_eq!(
        conversation_display_page(store.path(), &scope, None, 20, None)?
            .task_control
            .as_ref()
            .map(|task| task.task_id.as_str()),
        Some(task_id.as_str())
    );

    session.append_user_message(ModelMessage::user("explain an unrelated module"))?;
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        step_id: TaskStepId::new("late-step")?,
        role: AgentRole::SubagentRead,
        status: TaskStepStatus::Interrupted,
        title: None,
        summary: None,
        reason: Some("late background event".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "old durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("late background status".to_owned()),
    }))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert!(page.task_control.is_none());
    session.append_control(ControlEntry::TaskRunCancellationScopeBound(
        TaskRunCancellationScopeBoundEntry {
            task_id: task_id.clone(),
            run_scope_id: "display-explicit-focus-scope".to_owned(),
        },
    ))?;
    session.append_control(ControlEntry::TaskRunTargetSelected(
        TaskRunTargetSelectedEntry::new(
            task_id.clone(),
            "display-explicit-focus-scope",
            TaskRunStatus::Paused,
            Some(1),
            Some(TaskPlanStatus::Accepted),
        ),
    ))?;
    assert_eq!(
        conversation_display_page(store.path(), &scope, None, 20, None)?
            .task_control
            .as_ref()
            .map(|task| task.task_id.as_str()),
        Some(task_id.as_str())
    );
    Ok(())
}

#[test]
fn explicit_plan_draft_makes_paused_task_historical_after_reload() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let task_id = TaskId::new("task-historical-after-explicit-plan")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "old durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: Vec::new(),
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "old durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("waiting for follow-up".to_owned()),
    }))?;
    assert!(
        conversation_display_page(store.path(), &scope, None, 20, None)?
            .task_control
            .is_some()
    );

    session.append_control(ControlEntry::PlanDraftCreated(
        sigil_kernel::PlanDraftCreatedEntry {
            plan_id: sigil_kernel::PlanId::new("plan-explicit-after-task")?,
            schema_version: 2,
            source: sigil_kernel::PlanSourceRef::default(),
            plan_hash: "sha256:explicit-plan-after-task".to_owned(),
            summary: "Review an unrelated implementation plan".to_owned(),
            inline_text: None,
            steps: Vec::new(),
            intent_proposal: None,
            target_paths: Vec::new(),
            suggested_checks: Vec::new(),
            risk: None,
            notes: Vec::new(),
            workspace_snapshot_id: None,
            created_at_ms: 42,
        },
    ))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "old durable task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("late background status".to_owned()),
    }))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    assert!(
        page.task_control.is_none(),
        "a durable explicit plan draft must replace the old paused Task as conversation focus"
    );
    Ok(())
}

#[test]
fn durable_task_control_truncates_oversized_plan_summary_explicitly() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let task_id = TaskId::new("task-bounded-control")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "bounded projection".to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    let steps = (0..=MAX_CONVERSATION_TASK_CONTROL_ITEMS)
        .map(|index| {
            Ok(TaskStepSpec {
                step_id: TaskStepId::new(format!("step-{index}"))?,
                title: if index == 0 {
                    "x".repeat(MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES + 1)
                } else {
                    format!("Step {index}")
                },
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: if index == 0 {
                    (1..=MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS + 1)
                        .map(|dependency| TaskStepId::new(format!("step-{dependency}")))
                        .collect::<Result<Vec<_>>>()?
                } else {
                    Vec::new()
                },
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps,
        reason: None,
    }))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    let task = page
        .task_control
        .expect("paused Task control should project");
    assert_eq!(task.steps.len(), MAX_CONVERSATION_TASK_CONTROL_ITEMS);
    assert_eq!(
        task.steps[0].title.len(),
        MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES
    );
    assert_eq!(
        task.steps[0].depends_on.len(),
        MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS
    );
    assert!(task.steps_truncated);
    Ok(())
}

#[test]
fn durable_task_control_does_not_carry_step_status_across_plan_versions() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let task_id = TaskId::new("task-plan-version-control")?;
    let step_id = TaskStepId::new("shared-step-id")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "replan safely".to_owned(),
        title: None,

        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    for plan_version in [1, 2] {
        session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
            task_id: task_id.clone(),
            plan_version,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: format!("Plan {plan_version} step"),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            }],
            reason: None,
        }))?;
        if plan_version == 1 {
            session.append_control(ControlEntry::TaskStep(TaskStepEntry {
                task_id: task_id.clone(),
                plan_version,
                step_id: step_id.clone(),
                role: AgentRole::Executor,
                status: TaskStepStatus::Completed,
                title: None,
                summary: None,
                reason: None,
            }))?;
        }
    }
    session.append_control(ControlEntry::TaskStep(TaskStepEntry {
        task_id,
        plan_version: 1,
        step_id,
        role: AgentRole::Executor,
        status: TaskStepStatus::Interrupted,
        title: None,
        summary: None,
        reason: None,
    }))?;

    let page = conversation_display_page(store.path(), &scope, None, 20, None)?;
    let task = page
        .task_control
        .expect("paused Task control should project");
    assert_eq!(task.plan_version, Some(2));
    assert_eq!(task.steps[0].title, "Plan 2 step");
    assert!(task.steps[0].status.is_none());
    Ok(())
}

#[test]
fn plan_review_attempt_without_draft_still_projects_its_terminal_status() -> Result<()> {
    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let source =
        sigil_kernel::ConversationTurnRef::new(session.session_scope_id(), "message-1", "run-1")?;
    let review_id = sigil_kernel::plan_review_id_for_source(&source);
    let attempt_id = sigil_kernel::plan_review_attempt_id_for_review(&review_id);
    let plan_id = sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_id);
    let attempt_entry = |status: sigil_kernel::PlanReviewAttemptStatus,
                         terminal_reason: Option<sigil_kernel::PlanReviewTerminalReason>,
                         recorded_at_ms: u64| {
        sigil_kernel::PlanReviewAttemptEntry {
            plan_review_id: review_id.clone(),
            attempt_id: attempt_id.clone(),
            plan_id: plan_id.clone(),
            source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
            source_turn: source.clone(),
            route_decision_id: None,
            child_session_ref: sigil_kernel::plan_review_child_session_ref(&review_id, &attempt_id),
            finalizer_session_ref: None,
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            workspace_snapshot_id: None,
            pending_user_input: None,
            status,
            terminal_reason,
            recorded_at_ms,
        }
    };

    // A durable Started attempt with no committed draft must still project: the status is the
    // shared Planning lifecycle, draft details are absent, and no actions are offered.
    session.append_control(ControlEntry::PlanReviewAttempt(attempt_entry(
        sigil_kernel::PlanReviewAttemptStatus::Started,
        None,
        5,
    )))?;
    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    let started = page
        .plan_review
        .expect("a Started attempt must project instead of disappearing");
    assert_eq!(
        started.status,
        sigil_kernel::PublicPlanReviewStatus::Started
    );
    assert!(started.summary.is_none(), "no draft means no summary");
    assert!(started.plan_hash.is_none(), "no draft means no plan hash");
    assert!(
        started.allowed_actions.is_empty(),
        "no draft means no actions"
    );
    assert_eq!(started.plan_id, plan_id.as_str());

    // Loading a non-terminal attempt without an attached supervisor intentionally reconciles it
    // to Interrupted. Use independent durable fixtures for each terminal branch rather than
    // appending a second terminal fact to that recovered lifecycle.
    let project_terminal = |status: sigil_kernel::PlanReviewAttemptStatus,
                            reason: sigil_kernel::PlanReviewTerminalReason|
     -> Result<sigil_kernel::PublicPlanReview> {
        let (_temp, store, mut session) = durable_session()?;
        let scope = session.session_scope_id().to_owned();
        let source = sigil_kernel::ConversationTurnRef::new(
            session.session_scope_id(),
            "message-terminal",
            "run-terminal",
        )?;
        let review_id = sigil_kernel::plan_review_id_for_source(&source);
        let attempt_id = sigil_kernel::plan_review_attempt_id_for_review(&review_id);
        let plan_id = sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_id);
        let entry =
            |status, terminal_reason, recorded_at_ms| sigil_kernel::PlanReviewAttemptEntry {
                plan_review_id: review_id.clone(),
                attempt_id: attempt_id.clone(),
                plan_id: plan_id.clone(),
                source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
                source_turn: source.clone(),
                route_decision_id: None,
                child_session_ref: sigil_kernel::plan_review_child_session_ref(
                    &review_id,
                    &attempt_id,
                ),
                finalizer_session_ref: None,
                revision_request_id: None,
                attempt_ordinal: 1,
                base_plan_id: None,
                base_plan_hash: None,
                workspace_snapshot_id: None,
                pending_user_input: None,
                status,
                terminal_reason,
                recorded_at_ms,
            };
        session.append_control(ControlEntry::PlanReviewAttempt(entry(
            sigil_kernel::PlanReviewAttemptStatus::Started,
            None,
            5,
        )))?;
        session.append_control(ControlEntry::PlanReviewAttempt(entry(
            status,
            Some(reason),
            6,
        )))?;
        conversation_display_page(store.path(), &scope, None, 10, None)?
            .plan_review
            .context("terminal attempt must remain publicly visible")
    };

    // A durable terminal attempt without a draft (failed) also stays visible across reloads.
    let failed = project_terminal(
        sigil_kernel::PlanReviewAttemptStatus::Failed,
        sigil_kernel::PlanReviewTerminalReason::RunFailed,
    )?;
    assert_eq!(failed.status, sigil_kernel::PublicPlanReviewStatus::Failed);
    assert!(failed.summary.is_none());
    assert!(failed.allowed_actions.is_empty());

    // A cancelled attempt without a draft projects as cancelled.
    let cancelled = project_terminal(
        sigil_kernel::PlanReviewAttemptStatus::Cancelled,
        sigil_kernel::PlanReviewTerminalReason::UserCancelled,
    )?;
    assert_eq!(
        cancelled.status,
        sigil_kernel::PublicPlanReviewStatus::Cancelled
    );
    assert!(cancelled.allowed_actions.is_empty());
    Ok(())
}

#[derive(Debug, serde::Deserialize)]
struct LegacyPlanReviewFixtureV1 {
    schema_version: u16,
    source_session_id: String,
    redacted: bool,
    source: LegacyPlanReviewSourceFixtureV1,
    base: LegacyPlanReviewBaseFixtureV1,
    revision: LegacyPlanReviewRevisionFixtureV1,
    legacy_finalizer_evidence: LegacyPlanReviewFinalizerFixtureV1,
    expected: LegacyPlanReviewExpectedFixtureV1,
}

#[derive(Debug, serde::Deserialize)]
struct LegacyPlanReviewSourceFixtureV1 {
    session_scope_id: String,
    message_id: String,
    logical_run_id: String,
    route_decision_id: String,
    plan_review_id: String,
}

#[derive(Debug, serde::Deserialize)]
struct LegacyPlanReviewBaseFixtureV1 {
    attempt_id: String,
    plan_id: String,
    plan_hash: String,
    summary: String,
    step_title: String,
    draft_ready_at_ms: u64,
    revision_requested_at_ms: u64,
}

#[derive(Debug, serde::Deserialize)]
struct LegacyPlanReviewRevisionFixtureV1 {
    attempt_id: String,
    plan_id: String,
    started_at_ms: u64,
    terminal_at_ms: u64,
    terminal_status: sigil_kernel::PlanReviewAttemptStatus,
    terminal_reason: sigil_kernel::PlanReviewTerminalReason,
}

#[derive(Debug, serde::Deserialize)]
struct LegacyPlanReviewFinalizerFixtureV1 {
    attempted_tool: String,
    legacy_error: String,
    current_classification: String,
}

#[derive(Debug, serde::Deserialize)]
struct LegacyPlanReviewExpectedFixtureV1 {
    active_plan_status: sigil_kernel::PublicPlanReviewStatus,
    revision_status: sigil_kernel::PublicPlanRevisionStatusV1,
    retry_requires_guidance: bool,
}

fn legacy_plan_review_fixture_entries() -> Result<(
    LegacyPlanReviewFixtureV1,
    Vec<sigil_kernel::SessionLogEntry>,
)> {
    let fixture: LegacyPlanReviewFixtureV1 = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../dev/fixtures/plan-review-legacy-v1/session-5aeeb257-83fb-41c5-809b-68edcc0be15a.json"
    )))?;
    let source_turn = sigil_kernel::ConversationTurnRef::new(
        &fixture.source.session_scope_id,
        &fixture.source.message_id,
        &fixture.source.logical_run_id,
    )?;
    let route_decision_id =
        sigil_kernel::ConversationRouteDecisionId::new(fixture.source.route_decision_id.clone())?;
    let review_id = sigil_kernel::PlanReviewId::new(fixture.source.plan_review_id.clone())?;
    let base_attempt_id = sigil_kernel::PlanReviewAttemptId::new(fixture.base.attempt_id.clone())?;
    let base_plan_id = sigil_kernel::PlanId::new(fixture.base.plan_id.clone())?;
    let revision_attempt_id =
        sigil_kernel::PlanReviewAttemptId::new(fixture.revision.attempt_id.clone())?;
    let revision_plan_id = sigil_kernel::PlanId::new(fixture.revision.plan_id.clone())?;
    let attempt = |attempt_id: sigil_kernel::PlanReviewAttemptId,
                   plan_id: sigil_kernel::PlanId,
                   status: sigil_kernel::PlanReviewAttemptStatus,
                   terminal_reason: Option<sigil_kernel::PlanReviewTerminalReason>,
                   recorded_at_ms: u64| {
        sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::PlanReviewAttempt(
            sigil_kernel::PlanReviewAttemptEntry {
                plan_review_id: review_id.clone(),
                attempt_id: attempt_id.clone(),
                plan_id,
                source: sigil_kernel::PlanReviewSource::AutomaticConversationRoute,
                source_turn: source_turn.clone(),
                route_decision_id: Some(route_decision_id.clone()),
                child_session_ref: sigil_kernel::plan_review_child_session_ref(
                    &review_id,
                    &attempt_id,
                ),
                finalizer_session_ref: None,
                revision_request_id: None,
                attempt_ordinal: 1,
                base_plan_id: None,
                base_plan_hash: None,
                workspace_snapshot_id: None,
                pending_user_input: None,
                status,
                terminal_reason,
                recorded_at_ms,
            },
        ))
    };
    let entries = vec![
        attempt(
            base_attempt_id.clone(),
            base_plan_id.clone(),
            sigil_kernel::PlanReviewAttemptStatus::Started,
            None,
            fixture.base.draft_ready_at_ms.saturating_sub(1),
        ),
        sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::PlanDraftCreated(
            sigil_kernel::PlanDraftCreatedEntry {
                plan_id: base_plan_id.clone(),
                schema_version: 2,
                source: sigil_kernel::PlanSourceRef {
                    session_ref: None,
                    run_id: Some(fixture.source.logical_run_id.clone()),
                    final_message_id: None,
                    source_turn: Some(source_turn.clone()),
                    route_decision_id: Some(route_decision_id.clone()),
                    plan_review_id: Some(review_id.clone()),
                },
                plan_hash: fixture.base.plan_hash.clone(),
                summary: fixture.base.summary.clone(),
                inline_text: None,
                steps: vec![sigil_kernel::PlanDraftStep {
                    step_id: "legacy-step-1".to_owned(),
                    title: fixture.base.step_title.clone(),
                    display_name: None,
                    detail: Some("Redacted legacy plan detail remains reviewable.".to_owned()),
                    role: None,
                    depends_on: Vec::new(),
                    intent_aliases: Vec::new(),
                    mode: None,
                    isolation: None,
                    target_paths: vec!["crates/sigil-kernel/src/session".to_owned()],
                    required_capabilities: Vec::new(),
                    deliverables: Vec::new(),
                    acceptance_criteria: Vec::new(),
                    suggested_checks: Vec::new(),
                    risk: Some("medium".to_owned()),
                    notes: vec!["fixture content is redacted".to_owned()],
                }],
                intent_proposal: None,
                target_paths: vec!["crates/sigil-kernel/src/session".to_owned()],
                suggested_checks: Vec::new(),
                risk: Some("medium".to_owned()),
                notes: vec!["fixture content is redacted".to_owned()],
                workspace_snapshot_id: None,
                created_at_ms: fixture.base.draft_ready_at_ms.saturating_sub(1),
            },
        )),
        attempt(
            base_attempt_id,
            base_plan_id.clone(),
            sigil_kernel::PlanReviewAttemptStatus::DraftReady,
            None,
            fixture.base.draft_ready_at_ms,
        ),
        sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::PlanDecisionRecorded(
            sigil_kernel::PlanDecisionRecordedEntry {
                plan_id: base_plan_id,
                plan_hash: fixture.base.plan_hash.clone(),
                decision: sigil_kernel::PlanDecision::RevisionRequested,
                decided_by: sigil_kernel::PlanDecisionActor::User,
                decided_at_ms: fixture.base.revision_requested_at_ms,
                reason: Some("legacy revise plan".to_owned()),
            },
        )),
        attempt(
            revision_attempt_id.clone(),
            revision_plan_id.clone(),
            sigil_kernel::PlanReviewAttemptStatus::Started,
            None,
            fixture.revision.started_at_ms,
        ),
        attempt(
            revision_attempt_id,
            revision_plan_id,
            fixture.revision.terminal_status,
            Some(fixture.revision.terminal_reason),
            fixture.revision.terminal_at_ms,
        ),
    ];
    Ok((fixture, entries))
}

#[test]
fn legacy_session_5aeeb257_restores_the_base_plan_and_failed_revision() -> Result<()> {
    let (fixture, entries) = legacy_plan_review_fixture_entries()?;
    assert_eq!(fixture.schema_version, 1);
    assert_eq!(
        fixture.source_session_id,
        "5aeeb257-83fb-41c5-809b-68edcc0be15a"
    );
    assert!(fixture.redacted);
    assert_eq!(fixture.legacy_finalizer_evidence.attempted_tool, "grep");
    assert_eq!(
        fixture.legacy_finalizer_evidence.legacy_error,
        "unknown tool grep"
    );
    assert_eq!(
        fixture.legacy_finalizer_evidence.current_classification,
        sigil_kernel::PlanReviewTerminalReason::SubmitOnlyProtocolViolation.as_str()
    );

    assert_eq!(
        plan_review_compatibility_from_entries(&entries),
        PlanReviewCompatibilityStatusV1::LegacyRecovered
    );
    let review = public_plan_review_from_entries(&entries, None)
        .context("legacy review should remain publicly reviewable")?;
    assert_eq!(review.status, fixture.expected.active_plan_status);
    assert_eq!(review.plan_id, fixture.base.plan_id);
    assert_eq!(
        review.plan_hash.as_deref(),
        Some(fixture.base.plan_hash.as_str())
    );
    assert!(
        review
            .allowed_actions
            .contains(&sigil_kernel::PublicPlanAction::Revise)
    );
    assert!(fixture.expected.retry_requires_guidance);
    let revision = review
        .revision
        .context("legacy terminal revision should remain visible")?;
    assert_eq!(revision.status, fixture.expected.revision_status);
    assert_eq!(
        revision.attempt_id.as_deref(),
        Some(fixture.revision.attempt_id.as_str())
    );
    assert_eq!(
        revision.terminal_reason.as_deref(),
        Some(fixture.revision.terminal_reason.as_str())
    );
    Ok(())
}

#[test]
fn ambiguous_legacy_revision_lineage_fails_closed() -> Result<()> {
    let (_fixture, mut entries) = legacy_plan_review_fixture_entries()?;
    let Some(sigil_kernel::SessionLogEntry::Control(
        sigil_kernel::ControlEntry::PlanReviewAttempt(candidate),
    )) = entries.get_mut(4)
    else {
        anyhow::bail!("legacy fixture lost its candidate start");
    };
    candidate.source_turn =
        sigil_kernel::ConversationTurnRef::new("other-session", "other-message", "other-run")?;

    assert_eq!(
        plan_review_compatibility_from_entries(&entries),
        PlanReviewCompatibilityStatusV1::UnsupportedLegacy
    );
    let review = public_plan_review_from_entries(&entries, None)
        .context("unsupported legacy terminal should remain visible without authority")?;
    assert_eq!(
        review.status,
        sigil_kernel::PublicPlanReviewStatus::Interrupted
    );
    assert!(review.allowed_actions.is_empty());
    Ok(())
}
