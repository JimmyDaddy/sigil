use std::collections::BTreeSet;

use anyhow::Result;

use super::*;
use crate::{
    ConversationInputKind, ConversationInputQueueId, ConversationInputStatus,
    ConversationInputTarget, ModelMessage, ToolCall, conversation_promotion_capability_digest,
    project_conversation_prompt_for_persistence,
};

fn records(session: &Session) -> Result<Vec<SessionStreamRecord>> {
    let store = session
        .durable_store()
        .expect("safe-fold fixture must be store-backed");
    JsonlSessionStore::read_event_records(store.path())
}

fn store_backed_session() -> Result<(tempfile::TempDir, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    Ok((
        temp,
        Session::new("deepseek", "deepseek-v4-flash").with_store(store),
    ))
}

fn append_complete_turn(session: &mut Session, label: &str, payload_bytes: usize) -> Result<()> {
    session.append_user_message(ModelMessage::user(format!(
        "user-{label}-{}",
        "u".repeat(payload_bytes)
    )))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some(format!("assistant-{label}-{}", "a".repeat(payload_bytes))),
        Vec::new(),
    ))?;
    Ok(())
}

fn small_adaptive_policy() -> AdaptiveTailPolicyV3 {
    AdaptiveTailPolicyV3 {
        tail_min_complete_turns: 2,
        tail_target_min_tokens: 100,
        tail_target_max_tokens: 1_000,
        tail_recent_turn_p95_multiplier_ppm: 2_000_000,
        tail_max_usable_context_ratio_ppm: 250_000,
        recent_turn_sample_limit: 20,
        translated_legacy_tail_messages: Some(6),
    }
}

fn requested_tool_approval(call_id: &str) -> ControlEntry {
    ControlEntry::ToolApproval(ToolApprovalEntry {
        action: ToolApprovalAuditAction::Requested,
        call_id: call_id.to_owned(),
        tool_name: "shell".to_owned(),
        access: crate::ToolAccess::Read,
        network_effect: None,
        local_policy_decision: crate::ApprovalMode::Ask,
        network_policy_decision: crate::ApprovalMode::Allow,
        source_policy_decision: crate::ApprovalMode::Allow,
        operation: None,
        risk: None,
        subjects: Vec::new(),
        subject_zones: Vec::new(),
        policy_decision: crate::ApprovalMode::Ask,
        external_directory_required: false,
        confirmation: None,
        snapshot_required: false,
        command_permission_matches: Vec::new(),
        allow_source: None,
        grant_call_id: None,
        user_decision: None,
        reason: None,
        preview_hash: None,
    })
}

#[test]
fn safe_fold_plan_uses_durable_ids_and_preserves_tail_control_and_tool_pairs() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: r#"{\"path\":\"src/main.rs\"}"#.to_owned(),
    };
    session.append_assistant_message(ModelMessage::assistant(None, vec![tool_call]))?;
    session.append_tool_message(ModelMessage::tool("call-1", "file contents"))?;
    session.append_control(ControlEntry::UsageSnapshot(UsageStats::default()))?;
    session.append_user_message(ModelMessage::user("latest request"))?;

    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records(&stream, 1)?;

    assert_eq!(plan.schema_version, COMPACTION_FOLD_PLAN_SCHEMA_VERSION);
    assert_eq!(plan.folded_event_ids.len(), 3);
    assert_eq!(plan.retained_event_ids.len(), 1);
    assert_eq!(plan.protected_events.len(), 1);
    assert_eq!(
        plan.protected_events[0].reason,
        CompactionFoldProtectionReason::ControlState
    );
    assert_eq!(
        plan.folded_through
            .as_ref()
            .map(|cursor| cursor.through_event_id.as_str()),
        plan.folded_event_ids.last().map(String::as_str)
    );
    assert!(plan.validate_against(&stream).is_ok());
    Ok(())
}

#[test]
fn safe_fold_plan_expands_tail_to_keep_a_complete_tool_pair() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    };
    session.append_assistant_message(ModelMessage::assistant(None, vec![tool_call]))?;
    session.append_tool_message(ModelMessage::tool("call-1", "file contents"))?;

    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records(&stream, 1)?;

    assert_eq!(plan.folded_event_ids.len(), 1);
    assert_eq!(plan.retained_event_ids.len(), 2);
    assert!(plan.protected_events.is_empty());
    Ok(())
}

#[test]
fn v2_compaction_preview_is_read_only_and_reports_the_exact_fold_plan() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("old response".to_owned()),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user("latest request"))?;
    let store = session
        .durable_store()
        .expect("safe-fold fixture must be store-backed");
    let before = std::fs::read(store.path())?;

    let preview = store
        .v2_compaction_preview(1, None)?
        .expect("older messages should be foldable");

    assert_eq!(preview.plan.folded_event_ids.len(), 2);
    assert_eq!(preview.plan.retained_event_ids.len(), 1);
    assert!(preview.plan.protected_events.is_empty());
    assert!(preview.active_compaction_id.is_none());
    assert_eq!(std::fs::read(store.path())?, before);
    Ok(())
}

#[test]
fn safe_fold_plan_protects_unfinished_tool_pairs_and_rejects_stale_streams() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    let tool_call = ToolCall {
        id: "call-1".to_owned(),
        name: "read_file".to_owned(),
        args_json: "{}".to_owned(),
    };
    session.append_assistant_message(ModelMessage::assistant(None, vec![tool_call]))?;

    let before_append = records(&session)?;
    let plan = CompactionFoldPlan::from_records(&before_append, 1)?;
    assert_eq!(plan.folded_event_ids.len(), 1);
    assert_eq!(plan.protected_events.len(), 1);
    assert_eq!(
        plan.protected_events[0].reason,
        CompactionFoldProtectionReason::UnsafeToolPair
    );

    session.append_user_message(ModelMessage::user("new request"))?;
    assert!(plan.validate_against(&records(&session)?).is_err());
    Ok(())
}

#[test]
fn safe_fold_plan_never_folds_an_unpaired_tool_result() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_tool_message(ModelMessage::tool("missing-call", "orphan output"))?;
    session.append_user_message(ModelMessage::user("latest request"))?;

    let plan = CompactionFoldPlan::from_records(&records(&session)?, 1)?;
    assert_eq!(plan.folded_event_ids.len(), 1);
    assert_eq!(plan.retained_event_ids.len(), 1);
    assert_eq!(plan.protected_events.len(), 1);
    assert_eq!(
        plan.protected_events[0].reason,
        CompactionFoldProtectionReason::UnpairedToolResult
    );
    Ok(())
}

#[test]
fn safe_fold_plan_folds_delivered_promotion_at_its_durable_event_identity() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    let queue_id = ConversationInputQueueId::new("compaction-promoted")?;
    let prompt = project_conversation_prompt_for_persistence("old promoted request");
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
    let revision = session
        .try_conversation_queue_durable_projection_from_durable()?
        .expect("durable queue projection")
        .revision
        .expect("queued event advances revision");
    let mut durable_user_message = ModelMessage::user(prompt.safe_prompt.clone());
    durable_user_message.id = "compaction-promoted-message".to_owned();
    let store = session
        .durable_store()
        .expect("safe-fold fixture must be store-backed");
    let promotion = store.append_conversation_input_promoted(ConversationInputPromotedEntry {
        queue_id: queue_id.clone(),
        expected_queue_revision: revision,
        prompt_hash: prompt.prompt_hash,
        exact_prompt_required: false,
        durable_user_message,
        capability_descriptors: Vec::new(),
        capability_digest: conversation_promotion_capability_digest(&[])?,
        dispatch_run_id: "compaction-promoted-run".to_owned(),
        promoted_at_ms: 2,
    })?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ConversationInputStatusChanged(ConversationInputStatusEntry {
            queue_id,
            status: ConversationInputStatus::Delivered,
            reason: None,
            updated_at_ms: Some(3),
        }),
    ))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("old promoted response".to_owned()),
        Vec::new(),
    ))?;

    let stream = records(&session)?;
    assert_eq!(
        stream
            .iter()
            .filter(|record| {
                record.stored_event().event_kind() == Some(DurableEventType::UserMessageRecorded)
            })
            .count(),
        0
    );
    let plan = CompactionFoldPlan::from_records(&stream, 1)?;
    assert_eq!(plan.folded_event_ids, vec![promotion.event_id]);
    assert_eq!(plan.retained_event_ids.len(), 1);
    assert!(plan.validate_against(&stream).is_ok());
    Ok(())
}

#[test]
fn adaptive_tail_translates_legacy_messages_and_selects_complete_turns_by_p95_target() -> Result<()>
{
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..40 {
        append_complete_turn(&mut session, &index.to_string(), 128)?;
    }
    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &stream,
        AdaptiveTailPolicyV3::from_legacy_tail_messages(6),
        64 * 1024,
        None,
    )?;
    let adaptive = plan
        .adaptive_tail
        .as_ref()
        .expect("adaptive plan records its selection proof");

    assert_eq!(adaptive.policy.tail_min_complete_turns, 2);
    assert_eq!(adaptive.policy.translated_legacy_tail_messages, Some(6));
    assert!(adaptive.recent_complete_turn_p95_tokens > 0);
    assert_eq!(
        adaptive.ordinary_target_tokens,
        DEFAULT_TAIL_TARGET_MIN_TOKENS
    );
    assert!(adaptive.retained_complete_turns >= 2);
    assert!(
        adaptive
            .retained_turns
            .iter()
            .all(|turn| { turn.state == TailTurnStateV3::Complete && turn.event_ids.len() == 2 })
    );
    assert!(plan.has_foldable_history());
    assert!(plan.validate_against(&stream).is_ok());
    Ok(())
}

#[test]
fn adaptive_tail_clamps_p95_to_usable_context_ratio_deterministically() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..5 {
        append_complete_turn(&mut session, &index.to_string(), 100)?;
    }
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records(&session)?,
        small_adaptive_policy(),
        1_000,
        None,
    )?;
    let adaptive = plan.adaptive_tail.expect("adaptive selection");

    assert_eq!(adaptive.ordinary_target_tokens, 250);
    assert_eq!(adaptive.retained_complete_turns, 2);
    assert!(adaptive.retained_token_upper_bound <= adaptive.exact_fit_limit_tokens);
    Ok(())
}

#[test]
fn adaptive_tail_keeps_a_tool_heavy_active_turn_atomic_and_extends_target() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..3 {
        append_complete_turn(&mut session, &index.to_string(), 32)?;
    }
    session.append_user_message(ModelMessage::user("active tool-heavy request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "active-call".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_tool_message(ModelMessage::tool(
        "active-call",
        serde_json::json!({
            "status": "ok",
            "content": "x".repeat(4_000),
        })
        .to_string(),
    ))?;
    let stream = records(&session)?;
    let active_ids = stream
        .iter()
        .rev()
        .take(3)
        .map(|record| record.event_id().to_owned())
        .collect::<BTreeSet<_>>();
    let mut policy = small_adaptive_policy();
    policy.tail_target_min_tokens = 128;
    policy.tail_target_max_tokens = 128;
    let plan =
        CompactionFoldPlan::from_records_after_adaptive_tail(&stream, policy, 16 * 1024, None)?;
    let adaptive = plan.adaptive_tail.as_ref().expect("adaptive selection");
    let active_turn = adaptive
        .retained_turns
        .iter()
        .find(|turn| turn.state == TailTurnStateV3::Active)
        .expect("active turn remains raw");

    assert_eq!(
        active_turn
            .event_ids
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>(),
        active_ids
    );
    assert!(adaptive.active_turn_extended);
    assert!(active_turn.token_upper_bound > adaptive.ordinary_target_tokens);
    assert!(
        active_ids
            .iter()
            .all(|id| plan.retained_event_ids.contains(id))
    );
    assert!(
        active_ids
            .iter()
            .all(|id| !plan.folded_event_ids.contains(id))
    );
    assert!(plan.has_foldable_history());
    Ok(())
}

#[test]
fn adaptive_tail_rejects_an_active_turn_beyond_the_exact_fit_limit() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    append_complete_turn(&mut session, "old-1", 8)?;
    append_complete_turn(&mut session, "old-2", 8)?;
    session.append_user_message(ModelMessage::user("x".repeat(2_000)))?;

    let error = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records(&session)?,
        small_adaptive_policy(),
        1_000,
        None,
    )
    .expect_err("oversized active turn must reject ordinary compaction");
    assert!(error.to_string().contains("exact-fit limit"));
    Ok(())
}

#[test]
fn adaptive_tail_never_splits_a_waiting_approval_from_its_active_turn() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    for index in 0..3 {
        append_complete_turn(&mut session, &index.to_string(), 16)?;
    }
    session.append_user_message(ModelMessage::user("approve the active call"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "approval-call".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_tool_message(ModelMessage::tool(
        "approval-call",
        serde_json::json!({"status": "preview", "content": "pending"}).to_string(),
    ))?;
    session.append_control(requested_tool_approval("approval-call"))?;
    let stream = records(&session)?;
    let active_message_ids = stream
        .iter()
        .rev()
        .filter(|record| {
            matches!(
                session_entry_from_stored_event(record.stored_event()),
                Ok(Some(
                    SessionLogEntry::User(_)
                        | SessionLogEntry::Assistant(_)
                        | SessionLogEntry::ToolResult(_)
                        | SessionLogEntry::ToolResultV2(_)
                ))
            )
        })
        .take(3)
        .map(|record| record.event_id().to_owned())
        .collect::<BTreeSet<_>>();

    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &stream,
        small_adaptive_policy(),
        8 * 1024,
        None,
    )?;
    let adaptive = plan.adaptive_tail.as_ref().expect("adaptive selection");

    assert!(active_message_ids.iter().all(|id| {
        plan.protected_events.iter().any(|entry| {
            entry.event.event_id == *id
                && entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval
        })
    }));
    assert!(
        active_message_ids
            .iter()
            .all(|id| !plan.folded_event_ids.contains(id))
    );
    assert!(
        adaptive
            .protected_tail_events
            .iter()
            .any(|entry| entry.reason == CompactionFoldProtectionReason::ActiveToolOrApproval)
    );
    Ok(())
}
