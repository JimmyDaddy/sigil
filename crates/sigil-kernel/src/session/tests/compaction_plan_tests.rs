use anyhow::Result;

use super::*;
use crate::ToolCall;

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
