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
    assert_eq!(output.shrink.tool_name.as_deref(), Some("shell"));
    assert_eq!(output.shrink.status.as_deref(), Some("ok"));
    assert_eq!(output.candidate.tool_name, "shell");
    assert_eq!(output.candidate.tool_call_id, "call-1");
    assert_eq!(output.candidate.status, "ok");
    assert!(!output.candidate.head_excerpt.is_empty());
    assert!(!output.candidate.tail_excerpt.is_empty());
    assert_eq!(
        output.candidate.content_sha256,
        output
            .shrink
            .content_sha256
            .as_deref()
            .expect("content hash")
    );
    assert!(matches!(
        output.candidate.raw_durable_result,
        ToolOutputArtifactRefV1::DurableTranscriptEvent { .. }
    ));
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
    assert!(projected_content.contains("next-epoch recoverable tool output"));
    assert!(projected_content.contains("re_read_when_needed=true"));
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
    let current_context = session
        .try_context_projection_from_durable()?
        .expect("durable context");
    let current_tool = current_context
        .model_messages()
        .into_iter()
        .find(|message| message.tool_call_id.as_deref() == Some("call-1"))
        .expect("current epoch still contains the raw tool result");
    assert!(
        current_tool
            .content
            .as_deref()
            .is_some_and(|content| content.contains(&"middle-".repeat(600)))
    );
    assert!(
        current_tool
            .content
            .as_deref()
            .is_some_and(|content| !content.contains("next-epoch recoverable tool output"))
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
