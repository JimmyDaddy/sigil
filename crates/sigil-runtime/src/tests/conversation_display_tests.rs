use std::collections::HashSet;

use anyhow::Result;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use sigil_kernel::{
    AgentRole, ApprovalMode, AssistantMessageKind, CheckpointRestoreConflict,
    CheckpointRestoreConflictReason, ControlEntry, ConversationForked, ConversationInputKind,
    ConversationInputPromotedEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputTarget, ConversationRunFinalizedEntryV1, ConversationRunStartedEntryV1,
    ConversationRunTerminalStatusV1, DurableEventType, EventClass, JsonlSessionStore, MessageRole,
    ModelMessage, PermissionRisk, SecretRedactor, Session, SessionLogEntry, SessionRef,
    SessionStreamRecord, SkillLoadEntry, SkillSource, StoredEvent, TaskId, TaskIsolationMode,
    TaskPlanEntry, TaskPlanStatus, TaskRunEntry, TaskRunStatus, TaskStepEntry, TaskStepId,
    TaskStepMode, TaskStepSpec, TaskStepStatus, ToolAccess, ToolApprovalAuditAction,
    ToolApprovalDecisionReceiptV2, ToolApprovalEntry, ToolApprovalTerminalStatusV2,
    ToolApprovalUserDecision, ToolArtifactSensitivity, ToolArtifactStore, ToolCall, ToolOperation,
    ToolResult, ToolResultMeta, ToolResultRecordedV3, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
};

use crate::conversation_display::{
    ConversationDisplayAssistantPhaseV1, ConversationDisplayCheckpointConflictReasonV1,
    ConversationDisplayContentV1, ConversationDisplayItemKindV1, ConversationDisplayMessageRoleV1,
    ConversationDisplayProjectionError, ConversationDisplayStatusV1,
    ConversationLiveProvisionalSlotV1, MAX_CONVERSATION_DISPLAY_CONTENT_BYTES,
    MAX_CONVERSATION_DISPLAY_PAGE_BYTES, MAX_CONVERSATION_DISPLAY_PAGE_SIZE,
    MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS, MAX_CONVERSATION_TASK_CONTROL_ITEMS,
    MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES, conversation_display_page,
    conversation_display_page_from_records, conversation_live_provisional_id,
};

fn durable_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("provider", "model").with_store(store.clone());
    Ok((temp, store, session))
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

    let first = conversation_display_page(store.path(), &scope, None, 20)?;
    let second = conversation_display_page(store.path(), &scope, None, 20)?;
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

    let page = conversation_display_page(store.path(), &scope, None, 10)?;
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

    let page = conversation_display_page(store.path(), &scope, None, 10)?;
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
        conversation_display_page(store.path(), &scope, None, 20)
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
        conversation_display_page(store.path(), &scope, None, 20)
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

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
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

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
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

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
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

    let available = conversation_display_page(store.path(), &scope, None, 10)?;
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
    let missing = conversation_display_page(store.path(), &scope, None, 10)?;
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

    let first = conversation_display_page(store.path(), &scope, None, 2)?;
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
        conversation_display_page(store.path(), &scope, Some(&forged_cursor), 2)
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
    let second = conversation_display_page(store.path(), &scope, Some(&cursor), 2)?;
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
        conversation_display_page(store.path(), "another-scope", Some(&cursor), 2),
        Err(ConversationDisplayProjectionError::InvalidCursor { .. })
    ));
    let mut tampered = cursor;
    tampered.push('x');
    assert!(matches!(
        conversation_display_page(store.path(), &scope, Some(&tampered), 2),
        Err(ConversationDisplayProjectionError::InvalidCursor { .. })
    ));
    assert!(matches!(
        conversation_display_page(store.path(), &scope, Some("e30"), 2),
        Err(ConversationDisplayProjectionError::InvalidCursor { .. })
    ));

    let records = JsonlSessionStore::read_event_records(store.path())?;
    assert!(matches!(
        conversation_display_page_from_records(
            &records[..2],
            &scope,
            Some(&first.next_cursor.expect("cursor")),
            2
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

    let page = conversation_display_page(store.path(), &scope, None, 12)?;
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
    assert!(conversation_display_page(store.path(), &scope, None, 0).is_err());
    assert!(
        conversation_display_page(
            store.path(),
            &scope,
            None,
            MAX_CONVERSATION_DISPLAY_PAGE_SIZE + 1,
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

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
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

    let page = conversation_display_page_from_records(&[record], "scope-1", None, 10)?;
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
        conversation_display_page_from_records(&[unknown], "scope-1", None, 10)
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
        conversation_display_page_from_records(&[future_lifecycle], "scope-1", None, 10)
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
        conversation_display_page_from_records(&records, "scope-1", None, 10)
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
    let page = conversation_display_page(store.path(), &scope, None, 1)?;
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
        status: TaskRunStatus::Paused,
        reason: Some("private pause reason".to_owned()),
    }))?;

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
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
        status: TaskRunStatus::Completed,
        reason: None,
    }))?;
    let completed = conversation_display_page(store.path(), &scope, None, 20)?;
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
    let late_step = conversation_display_page(store.path(), &scope, None, 20)?;
    assert!(late_step.task_control.is_none());
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

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
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

    let page = conversation_display_page(store.path(), &scope, None, 20)?;
    let task = page
        .task_control
        .expect("paused Task control should project");
    assert_eq!(task.plan_version, Some(2));
    assert_eq!(task.steps[0].title, "Plan 2 step");
    assert!(task.steps[0].status.is_none());
    Ok(())
}
