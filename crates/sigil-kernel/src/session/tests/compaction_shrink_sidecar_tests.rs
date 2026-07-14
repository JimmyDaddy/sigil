use anyhow::Result;

use super::*;
use crate::ToolCall;

fn store_backed_session() -> Result<(tempfile::TempDir, JsonlSessionStore, Session)> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    Ok((temp, store, session))
}

fn records(store: &JsonlSessionStore) -> Result<Vec<SessionStreamRecord>> {
    JsonlSessionStore::read_event_records(store.path())
}

fn large_tool_message(call_id: &str) -> ModelMessage {
    ModelMessage::tool(
        call_id,
        serde_json::json!({
            "status": "ok",
            "content": format!("head:{}:tail", "middle-".repeat(1_000)),
        })
        .to_string(),
    )
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
            .expect("fixture has old foldable history"),
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
    session.append_tool_message(large_tool_message("call-1"))?;
    session.append_user_message(ModelMessage::user("latest request"))?;
    let source_records = records(&store)?;
    let plan = CompactionFoldPlan::from_records(&source_records, 1)?;
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
            .is_some_and(|content| content.contains("model_retrieval_available=false"))
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
            .is_some_and(|content| content.contains("old tool output projection"))
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
    session.append_tool_message(large_tool_message("call-1"))?;
    session.append_user_message(ModelMessage::user("latest request"))?;
    let source_records = records(&store)?;
    let plan = CompactionFoldPlan::from_records(&source_records, 1)?;
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
