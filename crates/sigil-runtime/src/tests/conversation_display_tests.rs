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
    JsonlSessionStore, MemoryConfig, MessageRole, ModelMessage, PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
    PermissionRisk, PublicEventOutboxEntryV1, PublicRunEvent, PublicRunEventKind,
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
    ConversationDisplayContentV1, ConversationDisplayIndex, ConversationDisplayItemKindV1,
    ConversationDisplayMessageRoleV1, ConversationDisplayPagePlan, ConversationDisplayPageV1,
    ConversationDisplayProjectionError, ConversationDisplayRecordPosition,
    ConversationDisplayStatusV1, ConversationLiveProvisionalSlotV1,
    MAX_CONVERSATION_DISPLAY_CONTENT_BYTES, MAX_CONVERSATION_DISPLAY_PAGE_BYTES,
    MAX_CONVERSATION_DISPLAY_PAGE_SIZE, MAX_CONVERSATION_TASK_CONTROL_DETAIL_ITEMS,
    MAX_CONVERSATION_TASK_CONTROL_ITEMS, MAX_CONVERSATION_TASK_CONTROL_TITLE_BYTES,
    conversation_display_page as canonical_display_page,
    conversation_display_page_from_records as canonical_display_page_from_records,
    conversation_live_provisional_id, public_plan_review_from_entries,
};

fn display_index(records: &[SessionStreamRecord]) -> Result<ConversationDisplayIndex> {
    let mut index = ConversationDisplayIndex::default();
    let mut offset = 0_u64;
    for record in records {
        let end = offset + u64::try_from(serde_json::to_vec(record.stored_event())?.len())? + 1;
        index.apply_record(
            record,
            ConversationDisplayRecordPosition {
                offset,
                end,
                sequence: record.stream_sequence(),
                checksum: record.record_checksum().to_owned(),
            },
        )?;
        offset = end;
    }
    Ok(index)
}

fn finish_indexed_page(
    plan: ConversationDisplayPagePlan,
    records: &[SessionStreamRecord],
    store: Option<&ToolArtifactStore>,
) -> Result<ConversationDisplayPageV1> {
    let raw_bytes = plan
        .positions()
        .iter()
        .map(|position| position.end - position.offset)
        .sum::<u64>();
    assert!(raw_bytes <= 4 * 1024 * 1024);
    let selected = plan
        .positions()
        .iter()
        .map(|position| {
            records[usize::try_from(position.sequence - 1).expect("fixture sequence")].clone()
        })
        .collect::<Vec<_>>();
    let page = plan.finish(&selected, store)?;
    assert!(serde_json::to_vec(&page)?.len() <= 1024 * 1024);
    Ok(page)
}

// Every successful canonical surface fixture below also exercises the production incremental
// index, including its hidden source reduction, reconciliation identities and cursor encoding.
fn conversation_display_page_from_records(
    records: &[SessionStreamRecord],
    scope: &str,
    cursor: Option<&str>,
    limit: usize,
    workspace: Option<&str>,
) -> std::result::Result<ConversationDisplayPageV1, ConversationDisplayProjectionError> {
    let page = canonical_display_page_from_records(records, scope, cursor, limit, workspace)?;
    let index = display_index(records)?;
    let indexed = finish_indexed_page(
        index.prepare_page(scope, cursor, limit, workspace)?,
        records,
        None,
    )?;
    assert_eq!(
        indexed, page,
        "incremental display must match canonical source projection"
    );
    Ok(page)
}

fn conversation_display_page(
    path: &std::path::Path,
    scope: &str,
    cursor: Option<&str>,
    limit: usize,
    workspace: Option<&str>,
) -> std::result::Result<ConversationDisplayPageV1, ConversationDisplayProjectionError> {
    let page = canonical_display_page(path, scope, cursor, limit, workspace)?;
    let records = JsonlSessionStore::read_event_records(path)?;
    let index = display_index(&records)?;
    let store = ToolArtifactStore::for_session_path(path);
    let indexed = finish_indexed_page(
        index.prepare_page(scope, cursor, limit, workspace)?,
        &records,
        Some(&store),
    )?;
    assert_eq!(
        indexed, page,
        "incremental display must match canonical source projection"
    );
    Ok(page)
}

fn durable_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let artifact_store = ToolArtifactStore::for_session_store(&store);
    let session = Session::new("provider", "model")
        .with_store(store.clone())
        .with_tool_artifact_store_override(artifact_store);
    Ok((temp, store, session))
}

fn synthetic_display_record(sequence: u64, entry: SessionLogEntry) -> Result<SessionStreamRecord> {
    let event_type = match &entry {
        SessionLogEntry::User(_) => DurableEventType::UserMessageRecorded,
        SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(_)) => {
            DurableEventType::SessionEntryRecorded
        }
        SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(_)) => {
            DurableEventType::PlanReviewAttempt
        }
        SessionLogEntry::Control(ControlEntry::PlanDraftCreated(_)) => {
            DurableEventType::PlanDraftCreated
        }
        SessionLogEntry::Control(ControlEntry::PlanDecisionRecorded(_)) => {
            DurableEventType::PlanDecisionRecorded
        }
        SessionLogEntry::Control(control)
            if sigil_kernel::UserInputLifecycleEntryV1::from_control(control).is_some() =>
        {
            DurableEventType::UserInputLifecycleChanged
        }
        SessionLogEntry::Control(_) => DurableEventType::TaskStatusChanged,
        _ => unreachable!("bounded source fixture only needs user and Task controls"),
    };
    let event_class = event_type
        .expected_event_class()
        .context("display fixture event type has no canonical class")?;
    let event = StoredEvent::new(
        event_type,
        event_class,
        format!("display-bound-{sequence}"),
        "scope-display-bound".to_owned(),
        sequence,
        json!({"session_log_entry": entry}),
    )?;
    event.to_json_line()?;
    Ok(SessionStreamRecord::Stored(event))
}

#[test]
fn conversation_display_index_accepts_verified_blank_gaps_and_rejects_overlap() -> Result<()> {
    let records = (1..=3)
        .map(|sequence| {
            synthetic_display_record(
                sequence,
                SessionLogEntry::User(ModelMessage::user(format!("message {sequence}"))),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let mut index = ConversationDisplayIndex::default();
    let mut offset = 2;
    for record in &records {
        let end = offset + u64::try_from(serde_json::to_vec(record.stored_event())?.len())? + 1;
        index.apply_record(
            record,
            ConversationDisplayRecordPosition {
                offset,
                end,
                sequence: record.stream_sequence(),
                checksum: record.record_checksum().to_owned(),
            },
        )?;
        offset = end + 3;
    }
    let first = finish_indexed_page(
        index.prepare_page("scope-display-bound", None, 1, None)?,
        &records,
        None,
    )?;
    let expected =
        canonical_display_page_from_records(&records, "scope-display-bound", None, 1, None)?;
    assert_eq!(first, expected);
    let second = finish_indexed_page(
        index.prepare_page("scope-display-bound", first.next_cursor.as_deref(), 1, None)?,
        &records,
        None,
    )?;
    assert_eq!(
        second,
        canonical_display_page_from_records(
            &records,
            "scope-display-bound",
            expected.next_cursor.as_deref(),
            1,
            None
        )?
    );
    let fourth = synthetic_display_record(4, SessionLogEntry::User(ModelMessage::user("overlap")))?;
    assert!(
        index
            .apply_record(
                &fourth,
                ConversationDisplayRecordPosition {
                    offset: offset - 4,
                    end: offset + 100,
                    sequence: 4,
                    checksum: fourth.record_checksum().to_owned(),
                }
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn conversation_display_index_rejects_noncurrent_cursors_without_rehashing_pages() -> Result<()> {
    let records = (1..=6)
        .map(|sequence| {
            synthetic_display_record(
                sequence,
                SessionLogEntry::User(ModelMessage::user(format!("message {sequence}"))),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let first =
        canonical_display_page_from_records(&records[..5], "scope-display-bound", None, 2, None)?;
    let cursor = first.next_cursor.context("fixed-frontier cursor missing")?;
    let current: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&cursor)?)?;
    assert_eq!(current["schema_version"], 2);
    let scope = "scope-display-bound";
    let index = display_index(&records)?;
    let initial_hash_work = index.hash_metrics();
    assert_eq!(initial_hash_work.prefix_records_hashed, 6);
    for version in [0, 1, 3] {
        let mut unsupported = current.clone();
        unsupported["schema_version"] = json!(version);
        let unsupported = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&unsupported)?);
        assert!(matches!(
            index
                .prepare_page(scope, Some(&unsupported), 2, None)
                .expect_err("noncurrent cursor"),
            crate::conversation_display::ConversationDisplayProjectionError::InvalidCursor { .. },
        ));
        assert!(matches!(
            canonical_display_page_from_records(&records, scope, Some(&unsupported), 2, None)
                .expect_err("noncurrent canonical cursor"),
            crate::conversation_display::ConversationDisplayProjectionError::InvalidCursor { .. },
        ));
    }
    let page = finish_indexed_page(
        index.prepare_page(scope, Some(&cursor), 2, None)?,
        &records,
        None,
    )?;
    assert_eq!(
        page,
        canonical_display_page_from_records(&records, scope, Some(&cursor), 2, None)?
    );
    assert_eq!(page.through_session_stream_sequence, 5);
    let next = page.next_cursor.context("current cursor missing")?;
    let next_json: serde_json::Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&next)?)?;
    assert_eq!(next_json["schema_version"], 2);
    let work = index.hash_metrics();
    for _ in 0..20 {
        let plan = index.prepare_page(scope, Some(&next), 2, None)?;
        assert!(plan.positions().is_empty());
        let page = finish_indexed_page(plan, &records, None)?;
        assert_eq!(
            page,
            canonical_display_page_from_records(&records, scope, Some(&next), 2, None)?
        );
        let latest = index.prepare_page(scope, None, 2, None)?;
        assert!(latest.positions().is_empty());
    }
    assert_eq!(
        index.hash_metrics(),
        work,
        "v2 fixed-cut and hot pages must not rehash historical metadata"
    );
    Ok(())
}

#[test]
fn conversation_display_index_shrinks_older_pages_to_the_raw_hydration_budget() -> Result<()> {
    let records = (1..=20)
        .map(|sequence| {
            synthetic_display_record(
                sequence,
                SessionLogEntry::User(ModelMessage::user(format!(
                    "{sequence:02}-{}",
                    "x".repeat(900_000)
                ))),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let index = display_index(&records)?;
    let hash_work = index.hash_metrics();
    let canonical =
        canonical_display_page_from_records(&records, "scope-display-bound", None, 10, None)?;
    let mut first = None;
    for _ in 0..20 {
        let plan = index.prepare_page("scope-display-bound", None, 10, None)?;
        assert!(
            plan.positions().is_empty(),
            "the bounded hot window must not reread transcript bodies"
        );
        let page = finish_indexed_page(plan, &records, None)?;
        assert_eq!(page, canonical);
        first = Some(page);
    }
    let first = first.context("hot page missing")?;
    let plan = index.prepare_page(
        "scope-display-bound",
        first.next_cursor.as_deref(),
        10,
        None,
    )?;
    assert_eq!(
        plan.positions().len(),
        4,
        "900 kB records must shrink the older page before hydration"
    );
    let second = finish_indexed_page(plan, &records, None)?;
    let canonical_second = canonical_display_page_from_records(
        &records,
        "scope-display-bound",
        first.next_cursor.as_deref(),
        10,
        None,
    )?;
    assert_eq!(
        second.items,
        canonical_second.items[canonical_second.items.len() - second.items.len()..]
    );
    let mut seen = first
        .items
        .iter()
        .chain(&second.items)
        .map(|item| item.display_id.clone())
        .collect::<HashSet<_>>();
    let mut cursor = second.next_cursor;
    while let Some(next) = cursor {
        let plan = index.prepare_page("scope-display-bound", Some(&next), 10, None)?;
        let page = finish_indexed_page(plan, &records, None)?;
        for item in &page.items {
            assert!(
                seen.insert(item.display_id.clone()),
                "shrunk pages must not duplicate rows"
            );
        }
        cursor = page.next_cursor;
    }
    assert_eq!(seen.len(), 20, "shrunk pages must not skip rows");
    assert_eq!(
        index.hash_metrics(),
        hash_work,
        "new cursors use cached prefix digests on hot and historical pages"
    );
    Ok(())
}

#[test]
fn conversation_display_index_caches_only_the_bounded_surface_of_a_large_task_source() -> Result<()>
{
    let private_detail = format!("PRIVATE_AGGREGATE_BODY_{}", "x".repeat(900_000));
    let task = ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("large-task-source")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("large-task-step")?,
            title: "Visible bounded title".to_owned(),
            display_name: None,
            detail: Some(private_detail),
            role: AgentRole::SubagentRead,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    });
    let records = vec![synthetic_display_record(1, SessionLogEntry::Control(task))?];
    let source_bytes = serde_json::to_vec(records[0].stored_event())?.len();
    assert!(source_bytes > 900_000);
    records[0].stored_event().to_json_line()?;
    assert!(source_bytes <= sigil_kernel::session::MAX_SESSION_RAW_RECORD_BYTES);
    let index = display_index(&records)?;
    assert!(!format!("{index:?}").contains("PRIVATE_AGGREGATE_BODY_"));
    let plan = index.prepare_page("scope-display-bound", None, 10, None)?;
    assert!(
        plan.positions().is_empty(),
        "large private aggregate source must use its bounded public cache"
    );
    let page = finish_indexed_page(plan, &records, None)?;
    assert_eq!(
        page,
        canonical_display_page_from_records(&records, "scope-display-bound", None, 10, None)?
    );
    assert_eq!(
        page.task_control.context("Task summary missing")?.steps[0].title,
        "Visible bounded title"
    );
    Ok(())
}

#[test]
fn conversation_display_index_rejects_an_indivisible_over_budget_summary() -> Result<()> {
    let mut records = Vec::new();
    for index in 0..64 {
        let options = (0..12).map(|option| json!({
            "id": format!("option-{option}"), "label": format!("Option {option}"), "description": "界".repeat(240)
        })).collect::<Vec<_>>();
        let questions = (0..2).map(|question| json!({
            "id": format!("question-{question}"), "header": "Choice", "question": "Choose one option", "required": true,
            "field": {"kind": "single_select", "options": options, "allow_other": false}
        })).collect::<Vec<_>>();
        let requested = sigil_kernel::UserInputRequestedV1::new(serde_json::from_value(json!({
            "schema_version": 1,
            "identity": {"session_scope_id": "scope-display-bound", "root_logical_run_id": format!("root-{index}"),
                "source_thread_id": format!("child-{index}"), "request_id": format!("request-{index}"), "generation": 1,
                "source_binding_hash": format!("sha256:{}", "a".repeat(64))},
            "source": "agent", "purpose": "clarification", "prompt": "界".repeat(500), "questions": questions,
            "allowed_actions": ["submit", "decline"], "requested_at_unix_ms": index + 10,
            "continuation": {"assistant_message_id": "assistant", "tool_call_id": "call", "provider_name": "test", "model_name": "model"}
        }))?)?;
        let public = sigil_kernel::UserInputRequestStateV1 {
            requested,
            status: sigil_kernel::UserInputStatusV1::Requested,
            decision: None,
            claim: None,
            continuation: None,
            resolution: None,
        }
        .public_view();
        let route: sigil_kernel::AgentUserInputRouteEntryV1 = serde_json::from_value(json!({
            "schema_version": 1, "route_id": format!("route-{index}"), "source_thread_id": format!("child-{index}"),
            "source_attempt_id": format!("attempt-{index}"), "profile_id": "explore", "parent_thread_id": "root", "batch_id": null,
            "budget_scope_id": "input-budget", "isolation": "shared_read_only", "child_session_ref": SessionRef::new_relative(format!("children/{index}.jsonl"))?,
            "request": public, "status": "requested", "updated_at_unix_ms": index + 10,
        }))?;
        route.validate()?;
        records.push(synthetic_display_record(
            index + 1,
            SessionLogEntry::Control(ControlEntry::AgentUserInputRoute(route)),
        )?);
    }
    let canonical =
        canonical_display_page_from_records(&records, "scope-display-bound", None, 10, None)?;
    assert!(
        serde_json::to_vec(&canonical)?.len() > 1024 * 1024,
        "64 individually legal forms exceed the aggregate response bound"
    );
    let index = display_index(&records)?;
    let plan = index.prepare_page("scope-display-bound", None, 10, None)?;
    let selected = plan
        .positions()
        .iter()
        .map(|position| {
            records[usize::try_from(position.sequence - 1).expect("fixture sequence")].clone()
        })
        .collect::<Vec<_>>();
    let error = plan.finish(&selected, None).expect_err(
        "an indivisible attention summary cannot silently exceed the response contract",
    );
    assert!(error.to_string().contains("1 MiB output budget"));
    Ok(())
}

#[test]
fn conversation_display_index_preserves_input_queue_lifecycle_at_old_frontiers() -> Result<()> {
    use sigil_kernel::{
        AgentProfileId, AgentRouteId, AgentRouteStatus, AgentRunAttemptId, AgentThreadId,
        AgentUserInputRouteEntryV1, LogicalRunId, SessionScopeId, UserInputActionV1,
        UserInputCommandId, UserInputContinuationBindingV1, UserInputDecisionAcceptedV1,
        UserInputDecisionV1, UserInputFieldKindV1, UserInputIdentityV1, UserInputPurposeV1,
        UserInputQuestionV1, UserInputRequestId, UserInputRequestStateV1, UserInputRequestV1,
        UserInputRequestedV1, UserInputResolutionV1, UserInputResolvedV1, UserInputSourceV1,
        UserInputStatusV1,
    };
    let request = UserInputRequestedV1::new(UserInputRequestV1 {
        schema_version: 1,
        identity: UserInputIdentityV1 {
            session_scope_id: SessionScopeId::new("scope-display-bound")?,
            root_logical_run_id: LogicalRunId::new("input-root")?,
            source_thread_id: AgentThreadId::new("input-child")?,
            request_id: UserInputRequestId::new("input-one")?,
            generation: 1,
            source_binding_hash: format!("sha256:{}", "a".repeat(64)),
        },
        source: UserInputSourceV1::Agent,
        purpose: UserInputPurposeV1::Clarification,
        prompt: "Direct request prompt".to_owned(),
        questions: vec![UserInputQuestionV1 {
            id: "scope".to_owned(),
            header: "Scope".to_owned(),
            question: "Which scope?".to_owned(),
            description: None,
            required: true,
            field: UserInputFieldKindV1::Text {
                multiline: false,
                max_chars: 256,
            },
        }],
        allowed_actions: vec![UserInputActionV1::Submit, UserInputActionV1::Decline],
        requested_at_unix_ms: 10,
        continuation: Some(UserInputContinuationBindingV1 {
            assistant_message_id: "input-assistant".to_owned(),
            tool_call_id: "input-call".to_owned(),
            provider_name: "test".to_owned(),
            model_name: "model".to_owned(),
        }),
    })?;
    let mut route = AgentUserInputRouteEntryV1 {
        schema_version: 1,
        route_id: AgentRouteId::new("input-route")?,
        source_thread_id: request.request.identity.source_thread_id.clone(),
        source_attempt_id: AgentRunAttemptId::new("input-attempt")?,
        profile_id: AgentProfileId::new("explore")?,
        parent_thread_id: AgentThreadId::new("root")?,
        batch_id: None,
        budget_scope_id: TaskId::new("input-budget")?,
        isolation: TaskIsolationMode::SharedReadOnly,
        child_session_ref: SessionRef::new_relative("children/input.jsonl")?,
        request: UserInputRequestStateV1 {
            requested: request.clone(),
            status: UserInputStatusV1::Requested,
            decision: None,
            claim: None,
            continuation: None,
            resolution: None,
        }
        .public_view(),
        status: AgentRouteStatus::Requested,
        updated_at_unix_ms: 10,
    };
    route.request.prompt = "Child route prompt".to_owned();
    let mut controls = vec![
        ControlEntry::UserInputRequested(Box::new(request.clone())),
        ControlEntry::AgentUserInputRoute(route.clone()),
    ];
    route.status = AgentRouteStatus::Registered;
    route.updated_at_unix_ms = 20;
    controls.push(ControlEntry::AgentUserInputRoute(route.clone()));
    controls.push(ControlEntry::UserInputDecisionAccepted(Box::new(
        UserInputDecisionAcceptedV1::new(
            &request,
            UserInputCommandId::new("input-decline")?,
            UserInputDecisionV1::Declined,
            30,
        )?,
    )));
    controls.push(ControlEntry::UserInputResolved(UserInputResolvedV1 {
        schema_version: 1,
        identity: request.request.identity.clone(),
        request_hash: request.request_hash.clone(),
        resolution: UserInputResolutionV1::Declined,
        resolved_at_unix_ms: 40,
    }));
    route.status = AgentRouteStatus::Resolved;
    route.updated_at_unix_ms = 50;
    controls.push(ControlEntry::AgentUserInputRoute(route));
    let mut records = vec![
        synthetic_display_record(1, SessionLogEntry::User(ModelMessage::user("one")))?,
        synthetic_display_record(2, SessionLogEntry::User(ModelMessage::user("two")))?,
    ];
    let mut old_pages = Vec::new();
    for control in controls {
        records.push(synthetic_display_record(
            u64::try_from(records.len())? + 1,
            SessionLogEntry::Control(control),
        )?);
        let page =
            conversation_display_page_from_records(&records, "scope-display-bound", None, 1, None)?;
        old_pages.push(page);
    }
    let index = display_index(&records)?;
    for old_page in old_pages {
        let cursor = old_page
            .next_cursor
            .context("fixed-frontier fixture needs one older row")?;
        let indexed = finish_indexed_page(
            index.prepare_page("scope-display-bound", Some(&cursor), 1, None)?,
            &records,
            None,
        )?;
        let canonical = canonical_display_page_from_records(
            &records,
            "scope-display-bound",
            Some(&cursor),
            1,
            None,
        )?;
        assert_eq!(indexed, canonical);
    }
    Ok(())
}

fn terminal_outbox(
    session_id: &str,
    run_id: &str,
    sequence: u64,
    event: PublicRunEventKind,
) -> Result<PublicEventOutboxEntryV1> {
    let public = PublicRunEvent::new(session_id, run_id, sequence, event);
    Ok(PublicEventOutboxEntryV1 {
        schema_version: PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION,
        public_event_id: format!("display-public:{run_id}:{sequence}"),
        domain_event_id: format!("display-domain:{run_id}:{sequence}"),
        run_id: run_id.to_owned(),
        sequence,
        payload_digest: sigil_kernel::stable_event_hash(&serde_json::to_vec(&public)?),
        event: public,
    })
}

fn internal_context_fixture() -> RuntimeContextCandidates {
    let body = "context snapshot body";
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
    assert!(!format!("{page:?}").contains("context snapshot body"));
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
    let terminal = ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some(final_message_id.clone()),
        Some("complete"),
        20,
        &SecretRedactor::empty(),
    )?;
    recorder.append_finalized_with_outbox(
        &terminal,
        &terminal_outbox(
            &scope,
            "run-1",
            1,
            PublicRunEventKind::RunFinished {
                final_text: "done".to_owned(),
            },
        )?,
    )?;

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
    let terminal = ConversationRunFinalizedEntryV1::new(
        "run-1",
        ConversationRunTerminalStatusV1::Succeeded,
        Some("another-message".to_owned()),
        Some("complete"),
        20,
        &SecretRedactor::empty(),
    )?;
    recorder.append_finalized_with_outbox(
        &terminal,
        &terminal_outbox(
            &scope,
            "run-1",
            1,
            PublicRunEventKind::RunFinished {
                final_text: "durable answer".to_owned(),
            },
        )?,
    )?;

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
fn plan_review_pending_inputs_follow_each_attempt_latest_generation_and_status() -> Result<()> {
    use sigil_kernel::{PlanReviewAttemptEntry, PlanReviewAttemptStatus, PublicUserInputRequestV1};

    let (_temp, store, mut session) = durable_session()?;
    let scope = session.session_scope_id().to_owned();
    let make_attempt = |label: &str| -> Result<PlanReviewAttemptEntry> {
        let source = sigil_kernel::ConversationTurnRef::new(&scope, label, label)?;
        let review = sigil_kernel::plan_review_id_for_source(&source);
        let attempt = sigil_kernel::plan_review_attempt_id_for_review(&review);
        Ok(PlanReviewAttemptEntry {
            plan_review_id: review.clone(),
            attempt_id: attempt.clone(),
            plan_id: sigil_kernel::plan_review_plan_id_for_attempt(&review, &attempt),
            source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
            source_turn: source,
            explicit_objective: Some("Resolve the plan question".to_owned()),
            route_decision_id: None,
            child_session_ref: sigil_kernel::plan_review_child_session_ref(&review, &attempt),
            finalizer_session_ref: None,
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            workspace_snapshot_id: None,
            pending_user_input: None,
            status: PlanReviewAttemptStatus::Started,
            terminal_reason: None,
            recorded_at_ms: 1,
        })
    };
    let question =
        |attempt: &PlanReviewAttemptEntry, generation: u32| -> Result<PublicUserInputRequestV1> {
            Ok(PublicUserInputRequestV1 {
                identity: sigil_kernel::UserInputIdentityV1 {
                    session_scope_id: sigil_kernel::SessionScopeId::new(format!(
                        "child-{}",
                        attempt.attempt_id.as_str()
                    ))?,
                    root_logical_run_id: sigil_kernel::LogicalRunId::new("review-run")?,
                    source_thread_id: sigil_kernel::AgentThreadId::new("main")?,
                    request_id: sigil_kernel::UserInputRequestId::new(format!(
                        "question-{generation}"
                    ))?,
                    generation,
                    source_binding_hash: format!("sha256:{}", "a".repeat(64)),
                },
                request_hash: format!("sha256:{generation:064x}"),
                source: sigil_kernel::UserInputSourceV1::PlanReviewResearch {
                    plan_review_id: attempt.plan_review_id.clone(),
                    attempt_id: attempt.attempt_id.clone(),
                },
                purpose: sigil_kernel::UserInputPurposeV1::Clarification,
                prompt: format!("Question generation {generation}"),
                questions: vec![sigil_kernel::UserInputQuestionV1 {
                    id: "scope".to_owned(),
                    header: "Scope".to_owned(),
                    question: "Which scope?".to_owned(),
                    description: None,
                    required: true,
                    field: sigil_kernel::UserInputFieldKindV1::Text {
                        multiline: false,
                        max_chars: 256,
                    },
                }],
                allowed_actions: vec![sigil_kernel::UserInputActionV1::Submit],
                requested_at_unix_ms: u64::from(generation),
                status: sigil_kernel::UserInputStatusV1::Requested,
                answer_receipt: None,
                resolution: None,
            })
        };
    let mut first = make_attempt("first-review")?;
    let mut second = make_attempt("second-review")?;
    let append = |session: &mut Session,
                  attempt: &mut PlanReviewAttemptEntry,
                  status,
                  pending|
     -> Result<()> {
        attempt.status = status;
        attempt.pending_user_input = pending;
        attempt.recorded_at_ms += 1;
        session.append_control(ControlEntry::PlanReviewAttempt(attempt.clone()))
    };
    session.append_control(ControlEntry::PlanReviewAttempt(first.clone()))?;
    session.append_control(ControlEntry::PlanReviewAttempt(second.clone()))?;
    let first_question = question(&first, 1)?;
    let second_question = question(&second, 4)?;
    append(
        &mut session,
        &mut first,
        PlanReviewAttemptStatus::WaitingForInput,
        Some(Box::new(first_question)),
    )?;
    append(
        &mut session,
        &mut second,
        PlanReviewAttemptStatus::WaitingForInput,
        Some(Box::new(second_question.clone())),
    )?;
    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert_eq!(page.user_inputs.len(), 2);

    append(
        &mut session,
        &mut first,
        PlanReviewAttemptStatus::Started,
        None,
    )?;
    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert_eq!(page.user_inputs, vec![second_question.clone()]);

    let next_question = question(&first, 2)?;
    append(
        &mut session,
        &mut first,
        PlanReviewAttemptStatus::WaitingForInput,
        Some(Box::new(next_question.clone())),
    )?;
    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert_eq!(
        page.user_inputs,
        vec![next_question, second_question.clone()]
    );

    append(
        &mut session,
        &mut first,
        PlanReviewAttemptStatus::Started,
        None,
    )?;
    append(
        &mut session,
        &mut first,
        PlanReviewAttemptStatus::DraftReady,
        None,
    )?;
    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert_eq!(page.user_inputs, vec![second_question]);
    append(
        &mut session,
        &mut second,
        PlanReviewAttemptStatus::Started,
        None,
    )?;
    append(
        &mut session,
        &mut second,
        PlanReviewAttemptStatus::DraftReady,
        None,
    )?;
    let page = conversation_display_page(store.path(), &scope, None, 10, None)?;
    assert!(page.user_inputs.is_empty());
    assert!(page.user_input.is_none());
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
            explicit_objective: Some("display plan review".to_owned()),
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
                explicit_objective: Some("terminal display plan review".to_owned()),
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
    assert_eq!(
        failed.allowed_actions,
        vec![sigil_kernel::PublicPlanAction::RetryReview]
    );

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
    source: LegacyPlanReviewSourceFixtureV1,
    base: LegacyPlanReviewBaseFixtureV1,
    revision: LegacyPlanReviewRevisionFixtureV1,
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
                explicit_objective: None,
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
fn old_plan_revision_is_rejected_instead_of_recovering_a_synthetic_request() -> Result<()> {
    let (_, entries) = legacy_plan_review_fixture_entries()?;
    for prefix in [&entries[..5], entries.as_slice()] {
        let error = public_plan_review_from_entries(prefix, None)
            .expect_err("old revision cannot expose a recovered plan");
        assert!(
            error
                .to_string()
                .contains("unsupported Plan revision format")
        );
        assert!(crate::conversation_display::public_user_inputs_from_entries(prefix).is_err());
        let records = prefix
            .iter()
            .enumerate()
            .map(|(index, entry)| synthetic_display_record(index as u64 + 1, entry.clone()))
            .collect::<Result<Vec<_>>>()?;
        assert!(
            canonical_display_page_from_records(&records, "scope-display-bound", None, 50, None,)
                .is_err()
        );
        assert!(display_index(&records).is_err());
    }
    Ok(())
}

#[test]
fn current_plan_revision_keeps_its_exact_request_and_base_after_terminal_failure() -> Result<()> {
    let (fixture, mut entries) = legacy_plan_review_fixture_entries()?;
    let request_id = sigil_kernel::UserInputRequestId::new("current-revision-request")?;
    for entry in &mut entries {
        if let SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) = entry {
            attempt.finalizer_session_ref = Some(sigil_kernel::plan_review_finalizer_session_ref(
                &attempt.plan_review_id,
                &attempt.attempt_id,
                1,
            ));
        }
    }
    for entry in &mut entries[4..] {
        let SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) = entry else {
            anyhow::bail!("revision fixture lost its attempt");
        };
        attempt.revision_request_id = Some(request_id.clone());
        attempt.base_plan_id = Some(sigil_kernel::PlanId::new(fixture.base.plan_id.clone())?);
        attempt.base_plan_hash = Some(fixture.base.plan_hash.clone());
    }
    entries.push(SessionLogEntry::Control(
        ControlEntry::PlanDecisionRecorded(sigil_kernel::PlanDecisionRecordedEntry {
            plan_id: sigil_kernel::PlanId::new(fixture.base.plan_id.clone())?,
            plan_hash: fixture.base.plan_hash.clone(),
            decision: sigil_kernel::PlanDecision::RevisionFailed,
            decided_by: sigil_kernel::PlanDecisionActor::System,
            decided_at_ms: fixture.revision.terminal_at_ms,
            reason: None,
        }),
    ));
    let review = public_plan_review_from_entries(&entries, None)?
        .context("current revision must keep its base visible")?;
    assert_eq!(review.plan_id, fixture.base.plan_id);
    assert_eq!(
        review.status,
        sigil_kernel::PublicPlanReviewStatus::DraftReady
    );
    assert!(
        review
            .allowed_actions
            .contains(&sigil_kernel::PublicPlanAction::Revise)
    );
    assert!(
        review
            .allowed_actions
            .contains(&sigil_kernel::PublicPlanAction::Run)
    );
    let revision = review
        .revision
        .context("exact revision must remain visible")?;
    assert_eq!(revision.request_id, request_id.as_str());
    assert_eq!(
        revision.attempt_id.as_deref(),
        Some(fixture.revision.attempt_id.as_str())
    );
    assert_eq!(
        revision.status,
        sigil_kernel::PublicPlanRevisionStatusV1::Failed
    );
    Ok(())
}
