use anyhow::Result;

use super::*;
use crate::ToolCall;

fn store_backed_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    append_current_test_session_identity(&store)?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    Ok((temp, store, session))
}

fn records(store: &JsonlSessionStore) -> Result<Vec<SessionStreamRecord>> {
    JsonlSessionStore::read_event_records(store.path())
}

fn large_tool_result(call_id: &str) -> ToolResult {
    ToolResult::ok(
        call_id,
        "shell",
        serde_json::json!({
            "status": "ok",
            "content": format!("head:{}:tail", "middle-".repeat(1_000)),
        })
        .to_string(),
        ToolResultMeta::default(),
    )
}

fn compact_test_policy() -> AdaptiveTailPolicyV3 {
    AdaptiveTailPolicyV3 {
        tail_target_min_tokens: 1,
        tail_target_max_tokens: 1,
        ..AdaptiveTailPolicyV3::default()
    }
}

fn append_followup_tail(session: &mut Session, label: &str) -> Result<()> {
    session.append_user_message(ModelMessage::user(format!("{label} follow-up one")))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some(format!("{label} response one")),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user(format!("{label} follow-up two")))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some(format!("{label} response two")),
        Vec::new(),
    ))?;
    session.append_user_message(ModelMessage::user(format!("{label} active request")))?;
    Ok(())
}

fn started() -> CompactionStartedEntry {
    CompactionStartedEntry {
        attempt_id: "attempt-shrink".to_owned(),
        fallback_parent: CompactionFallbackParent::Root,
        initiation: CompactionInitiation::Manual,
        base_projection_revision: "projection-r1".to_owned(),
        started_at_unix_ms: 1,
    }
}

fn applied(plan: &CompactionFoldPlan) -> CompactionAppliedV2 {
    CompactionAppliedV2 {
        compaction_id: "compaction-shrink".to_owned(),
        attempt_id: "attempt-shrink".to_owned(),
        parent_compaction_id: None,
        branch_id: None,
        valid_for_snapshot: None,
        task_memory_id: None,
        checkpoint: ContinuationCheckpointV1::empty(),
        base_projection_revision: "projection-r1".to_owned(),
        folded_through: plan
            .folded_through
            .clone()
            .expect("fixture has foldable history"),
        applied_at_unix_ms: 2,
    }
}

#[test]
fn shrink_sidecar_binds_to_applied_compaction_and_rebuilds_from_raw_history() -> Result<()> {
    let (_temp, store, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_test_tool_result(large_tool_result("call-1"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("semantic tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session, "semantic")?;
    let source_records = records(&store)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &source_records,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
    let policy = ToolOutputProjectionPolicy {
        max_projected_content_bytes: 512,
        retained_head_bytes: 200,
        retained_tail_bytes: 200,
    };
    let projection = ToolOutputProjection::from_fold_plan(&source_records, &plan, &policy)?;
    assert_eq!(projection.outputs.len(), 1);

    let start = store.append_compaction_started(started())?;
    let applied = store.append_compaction_applied_v2(applied(&plan))?;
    let entry = ToolOutputProjectionShrinkRecorded::from_projection(
        "compaction-shrink",
        "attempt-shrink",
        &plan,
        policy,
        &projection,
    )?;
    assert!(
        entry.epoch_transition.reason
            == ToolOutputContextEpochTransitionReasonV1::SemanticCompaction
            && entry.epoch_transition.source_epoch_id != entry.epoch_transition.target_epoch_id
    );
    let sidecar = store.append_tool_output_projection_shrink_recorded(entry)?;
    assert_eq!(
        sidecar.correlation_id.as_deref(),
        Some(start.event_id.as_str())
    );
    assert_eq!(
        sidecar.causation_id.as_deref(),
        Some(applied.event_id.as_str())
    );

    let stream = records(&store)?;
    let rebuilt = ToolOutputProjectionSidecarProjection::from_records(&stream)?;
    let outputs = rebuilt
        .outputs_for_compaction("compaction-shrink")
        .expect("applied compaction has a shrink sidecar");
    assert_eq!(outputs.len(), 1);
    assert_eq!(outputs[0].shrink, projection.outputs[0].shrink);
    assert!(
        outputs[0]
            .message
            .content
            .as_deref()
            .is_some_and(|content| {
                content.contains("model_retrieval_available=true")
                    && content.contains("use_read_tool_artifact=true")
            })
    );
    let typed = stream
        .last()
        .expect("sidecar appended")
        .typed_domain_event_record()?
        .expect("sidecar is typed");
    assert!(matches!(
        typed.event,
        TypedDomainEvent::ToolOutputProjectionShrinkRecorded(_)
    ));
    let context = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    let projected_tool = context
        .model_messages()
        .into_iter()
        .find(|message| message.tool_call_id.as_deref() == Some("call-1"))
        .expect("projected context retains the completed tool result");
    assert!(
        projected_tool
            .content
            .as_deref()
            .is_some_and(|content| content.contains("next-epoch artifact-backed tool output"))
    );
    Ok(())
}

#[test]
fn standalone_tool_output_shrink_requires_a_distinct_context_epoch() -> Result<()> {
    let transition =
        ToolOutputContextEpochTransitionV1::standalone("context-epoch:one", "context-epoch:two")?;
    assert_eq!(
        transition.reason,
        ToolOutputContextEpochTransitionReasonV1::StandaloneShrink
    );
    assert!(
        ToolOutputContextEpochTransitionV1::standalone("context-epoch:same", "context-epoch:same")
            .is_err()
    );
    Ok(())
}

#[test]
fn standalone_tool_output_shrink_rotates_projection_without_semantic_checkpoint() -> Result<()> {
    let (_temp, store, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-standalone".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    let raw_tool = large_tool_result("call-standalone");
    let raw_content = raw_tool.content.clone();
    session.append_test_tool_result(raw_tool)?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("standalone tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session, "standalone")?;
    let source_records = records(&store)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &source_records,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
    let appended = store
        .append_standalone_tool_output_projection(
            "context-epoch:raw",
            "context-epoch:standalone:test",
            plan,
            ToolOutputProjectionPolicy {
                max_projected_content_bytes: 512,
                retained_head_bytes: 200,
                retained_tail_bytes: 200,
            },
        )?
        .expect("fixture has one standalone shrink candidate");
    assert_eq!(
        appended.correlation_id.as_deref(),
        Some(appended.event_id.as_str())
    );
    assert!(appended.causation_id.is_none());

    let context = session
        .try_context_projection_from_durable()?
        .expect("store-backed session has a durable projection");
    assert!(context.active_compaction_id.is_none());
    let projected = context
        .model_messages()
        .into_iter()
        .find(|message| message.tool_call_id.as_deref() == Some("call-standalone"))
        .expect("standalone projection retains the tool result");
    assert_ne!(projected.content.as_deref(), Some(raw_content.as_str()));
    assert!(projected.content.as_deref().is_some_and(|content| {
        content.contains("next-epoch artifact-backed tool output")
            && content.contains("use_read_tool_artifact=true")
    }));

    let durable_json = std::fs::read_to_string(store.path())?;
    assert!(durable_json.contains("middle-middle-middle"));
    assert!(durable_json.contains("standalone_shrink"));
    Ok(())
}

#[test]
fn repeated_standalone_shrink_records_only_new_historical_outputs() -> Result<()> {
    let (_temp, store, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request one"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-standalone-one".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_test_tool_result(large_tool_result("call-standalone-one"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("first tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session, "first")?;

    let first_records = records(&store)?;
    let first_plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &first_records,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
    store
        .append_standalone_tool_output_projection(
            "context-epoch:root",
            "context-epoch:standalone:one",
            first_plan,
            ToolOutputProjectionPolicy {
                max_projected_content_bytes: 512,
                retained_head_bytes: 200,
                retained_tail_bytes: 200,
            },
        )?
        .expect("first historical tool output is eligible");

    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-standalone-two".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_test_tool_result(large_tool_result("call-standalone-two"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("second tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session, "second")?;

    let second_records = records(&store)?;
    let second_plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &second_records,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
    let second = store
        .append_standalone_tool_output_projection(
            "context-epoch:standalone:one",
            "context-epoch:standalone:two",
            second_plan,
            ToolOutputProjectionPolicy {
                max_projected_content_bytes: 512,
                retained_head_bytes: 200,
                retained_tail_bytes: 200,
            },
        )?
        .expect("only the newly historical tool output is eligible");
    let second_entry: ToolOutputProjectionShrinkRecorded =
        serde_json::from_value(second.payload.clone())?;
    assert_eq!(second_entry.shrinks.len(), 1);
    assert_eq!(second_entry.shrinks[0].tool_call_id, "call-standalone-two");

    let final_records = records(&store)?;
    let final_projection = ToolOutputProjectionSidecarProjection::from_records(&final_records)?;
    assert_eq!(
        final_projection.active_standalone_source_event_ids().len(),
        2
    );
    let final_plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &final_records,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
    assert!(
        store
            .append_standalone_tool_output_projection(
                "context-epoch:standalone:two",
                "context-epoch:standalone:three",
                final_plan,
                ToolOutputProjectionPolicy {
                    max_projected_content_bytes: 512,
                    retained_head_bytes: 200,
                    retained_tail_bytes: 200,
                },
            )?
            .is_none(),
        "an already projected source must not be recorded again"
    );
    Ok(())
}

#[test]
fn shrink_sidecar_rejects_tampered_descriptor_before_persistence() -> Result<()> {
    let (_temp, store, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_test_tool_result(large_tool_result("call-1"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("tamper fixture tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session, "tamper")?;
    let source_records = records(&store)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &source_records,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
    let policy = ToolOutputProjectionPolicy {
        max_projected_content_bytes: 512,
        retained_head_bytes: 200,
        retained_tail_bytes: 200,
    };
    let projection = ToolOutputProjection::from_fold_plan(&source_records, &plan, &policy)?;
    store.append_compaction_started(started())?;
    store.append_compaction_applied_v2(applied(&plan))?;
    let mut entry = ToolOutputProjectionShrinkRecorded::from_projection(
        "compaction-shrink",
        "attempt-shrink",
        &plan,
        policy,
        &projection,
    )?;
    entry.shrinks[0].omitted_bytes += 1;
    assert!(
        store
            .append_tool_output_projection_shrink_recorded(entry)
            .is_err()
    );
    assert_eq!(records(&store)?.len(), source_records.len() + 2);
    Ok(())
}
