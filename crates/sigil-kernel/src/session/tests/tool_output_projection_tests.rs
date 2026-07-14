use anyhow::Result;
use serde_json::Value;

use super::*;
use crate::ToolCall;

fn records(session: &Session) -> Result<Vec<SessionStreamRecord>> {
    let store = session
        .durable_store()
        .expect("tool-output projection fixture must be store-backed");
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

fn large_tool_message(call_id: &str) -> ModelMessage {
    let content = format!("head:{}:tail", "middle-".repeat(1_000));
    ModelMessage::tool(
        call_id,
        serde_json::json!({"status": "ok", "content": content, "meta": {"exit_code": 0}})
            .to_string(),
    )
}

#[test]
fn old_completed_tool_output_shrinks_only_in_projection_with_truthful_metadata() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-1".to_owned(),
            name: "shell".to_owned(),
            args_json: r#"{\"command\":\"rg TODO\"}"#.to_owned(),
        }],
    ))?;
    session.append_tool_message(large_tool_message("call-1"))?;
    session.append_user_message(ModelMessage::user("latest request"))?;
    let before = std::fs::read(session.durable_store().expect("fixture has store").path())?;

    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records(&stream, 1)?;
    let projection = ToolOutputProjection::from_fold_plan(
        &stream,
        &plan,
        &ToolOutputProjectionPolicy {
            max_projected_content_bytes: 512,
            retained_head_bytes: 200,
            retained_tail_bytes: 200,
        },
    )?;

    assert_eq!(projection.outputs.len(), 1);
    let output = &projection.outputs[0];
    assert_eq!(output.message.tool_call_id.as_deref(), Some("call-1"));
    assert!(output.shrink.omitted_bytes > 0);
    let envelope: Value = serde_json::from_str(
        output
            .message
            .content
            .as_deref()
            .expect("projected tool output remains structured"),
    )?;
    assert_eq!(envelope["status"], "ok");
    assert_eq!(
        envelope["compaction_projection"]["source_ref"]["model_retrieval_available"],
        false
    );
    assert_eq!(
        envelope["compaction_projection"]["source_ref"]["event_id"],
        output.shrink.source_event.event_id
    );
    let projected_content = envelope["content"]
        .as_str()
        .expect("projected tool content is text");
    assert!(projected_content.len() <= 512);
    assert!(projected_content.contains("old tool output projection"));
    assert!(projected_content.contains(&format!(
        "retained_head_bytes={}",
        output.shrink.retained_head_bytes
    )));
    assert!(projected_content.contains(&format!(
        "retained_tail_bytes={}",
        output.shrink.retained_tail_bytes
    )));
    assert!(projected_content.contains(&format!("omitted_bytes={}", output.shrink.omitted_bytes)));
    assert!(!projected_content.contains(&"middle-".repeat(600)));
    assert_eq!(
        std::fs::read(session.durable_store().expect("fixture has store").path(),)?,
        before
    );
    Ok(())
}

#[test]
fn tail_tool_pair_and_unpaired_tool_output_never_shrink() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
    session.append_user_message(ModelMessage::user("old request"))?;
    session.append_tool_message(large_tool_message("missing-call"))?;
    session.append_assistant_message(ModelMessage::assistant(
        None,
        vec![ToolCall {
            id: "call-2".to_owned(),
            name: "shell".to_owned(),
            args_json: "{}".to_owned(),
        }],
    ))?;
    session.append_tool_message(large_tool_message("call-2"))?;

    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records(&stream, 1)?;
    let projection = ToolOutputProjection::from_fold_plan(
        &stream,
        &plan,
        &ToolOutputProjectionPolicy {
            max_projected_content_bytes: 512,
            retained_head_bytes: 200,
            retained_tail_bytes: 200,
        },
    )?;

    assert!(projection.outputs.is_empty());
    assert_eq!(plan.protected_events.len(), 1);
    assert_eq!(plan.retained_event_ids.len(), 2);
    Ok(())
}

#[test]
fn stale_fold_plan_cannot_produce_a_tool_output_projection() -> Result<()> {
    let (_temp, mut session) = store_backed_session()?;
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
    let plan = CompactionFoldPlan::from_records(&records(&session)?, 1)?;

    session.append_user_message(ModelMessage::user("new request"))?;
    assert!(
        ToolOutputProjection::from_fold_plan(
            &records(&session)?,
            &plan,
            &ToolOutputProjectionPolicy::default(),
        )
        .is_err()
    );
    Ok(())
}
