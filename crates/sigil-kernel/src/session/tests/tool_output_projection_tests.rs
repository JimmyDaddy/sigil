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

fn large_tool_result(call_id: &str) -> ToolResult {
    let content = format!("head:{}:tail", "middle-".repeat(1_000));
    ToolResult::ok(
        call_id,
        "shell",
        serde_json::json!({"status": "ok", "content": content, "meta": {"exit_code": 0}})
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

fn append_followup_tail(session: &mut Session) -> Result<()> {
    for index in 0..2 {
        session.append_user_message(ModelMessage::user(format!("follow-up {index}")))?;
        session.append_assistant_message(ModelMessage::assistant(
            Some(format!("response {index}")),
            Vec::new(),
        ))?;
    }
    session.append_user_message(ModelMessage::user("active request"))?;
    Ok(())
}

#[test]
fn historical_completed_tool_output_shrinks_only_in_projection_with_truthful_metadata() -> Result<()>
{
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
    session.append_test_tool_result(large_tool_result("call-1"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session)?;
    let before = std::fs::read(session.durable_store().expect("fixture has store").path())?;

    let stream = records(&session)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &stream,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;
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
        ToolOutputArtifactRefV1::PublishedArtifact { .. }
    ));
    let envelope: Value = serde_json::from_str(
        output
            .message
            .content
            .as_deref()
            .expect("projected tool output remains structured"),
    )?;
    assert_eq!(envelope["facts"]["status"], "ok");
    let projected_content = envelope["projection"]["preview"]
        .as_str()
        .expect("projected tool content is text");
    assert!(projected_content.len() <= 512);
    assert!(projected_content.contains("next-epoch artifact-backed tool output"));
    assert!(projected_content.contains("use_read_tool_artifact=true"));
    assert!(projected_content.contains("model_retrieval_available=true"));
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
            .is_some_and(|content| !content.contains("next-epoch artifact-backed tool output"))
    );
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
    session.append_test_tool_result(large_tool_result("call-1"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("tool turn complete".to_owned()),
        Vec::new(),
    ))?;
    append_followup_tail(&mut session)?;
    let plan = CompactionFoldPlan::from_records_after_adaptive_tail(
        &records(&session)?,
        compact_test_policy(),
        u64::MAX / 4,
        None,
    )?;

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
