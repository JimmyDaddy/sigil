use std::fs;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    ControlEntry, DurableEventType, EventClass, JsonlSessionStore, ModelMessage, Session,
    SessionRef,
};

use super::*;
use crate::LocalSessionLifecycleService;

fn finalized_session(path: &Path) -> Result<(String, String)> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
    })?;
    let session_id = session.session_scope_id().to_owned();
    session.append_user_message(ModelMessage::user("inspect the parser"))?;
    let assistant = ModelMessage::assistant(Some("done".to_owned()), Vec::new());
    session.append_assistant_message(assistant.clone())?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": "completed",
            "terminal_reason": "final_answer",
            "final_message_id": assistant.id,
            "tool_calls": 0,
            "error": null
        }),
    )?;
    let view = application_conversation_recovery_view(path, &session_id)?;
    Ok((
        session_id,
        view.fork_points
            .last()
            .expect("finalized turn")
            .source_turn_digest
            .clone(),
    ))
}

#[test]
fn recovery_view_rejects_scope_drift_and_projects_exact_fork_binding() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("source.jsonl");
    let (session_id, digest) = finalized_session(&path)?;

    let view = application_conversation_recovery_view(&path, &session_id)?;
    assert!(view.checkpoints.is_empty());
    assert_eq!(view.fork_points.len(), 1);
    assert_eq!(view.fork_points[0].source_turn_digest, digest);
    assert!(view.through_stream_sequence > 0);
    assert!(application_conversation_recovery_view(&path, "other-scope").is_err());
    Ok(())
}

#[test]
fn lifecycle_fork_keeps_parent_unchanged_and_rejects_stale_turn_digest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    let exports = temp.path().join("exports");
    fs::create_dir_all(&sessions)?;
    let source = sessions.join("source.jsonl");
    let (session_id, digest) = finalized_session(&source)?;
    let before = fs::read(&source)?;
    let service = LocalSessionLifecycleService::new("workspace", &sessions, exports);
    let session_ref = SessionRef::new_relative("source.jsonl")?;

    assert!(
        service
            .fork_session_at_turn(&session_ref, &session_id, "stale", "command-42")
            .is_err()
    );
    let output = service.fork_session_at_turn(&session_ref, &session_id, &digest, "command-42")?;
    let replay = service.fork_session_at_turn(&session_ref, &session_id, &digest, "command-42")?;

    assert_eq!(fs::read(&source)?, before);
    assert!(output.destination_path.exists());
    assert_eq!(output.copied_message_count, 2);
    assert_ne!(output.destination_session_id, session_id);
    assert_eq!(replay.destination_session_id, output.destination_session_id);
    Ok(())
}
