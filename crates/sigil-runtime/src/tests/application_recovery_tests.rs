use std::fs;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    ControlEntry, DurableEventType, EventClass, JsonlSessionStore, ModelMessage,
    ResolvedModelRoute, RootConfig, Session, SessionRef,
};

use super::*;
use crate::LocalSessionLifecycleService;

fn finalized_session(path: &Path) -> Result<(String, String)> {
    finalized_session_with_route(path, None)
}

fn finalized_session_with_route(
    path: &Path,
    resolved_model_route: Option<ResolvedModelRoute>,
) -> Result<(String, String)> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route,
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
fn lifecycle_fork_rebinds_a_stale_source_route_to_the_current_default() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions)?;
    let root_config = deepseek_root_config(temp.path())?;
    let (_, current_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;
    let stale_route = ResolvedModelRoute::new(
        current_route.model_ref.clone(),
        current_route.provider_family.clone(),
        current_route.protocol.clone(),
        "stale-semantic-fingerprint",
    )?;
    let source = sessions.join("stale-source.jsonl");
    let (session_id, digest) = finalized_session_with_route(&source, Some(stale_route.clone()))?;
    let service =
        LocalSessionLifecycleService::new("workspace", &sessions, temp.path().join("exports"));

    let output = service.fork_session_at_turn(
        &SessionRef::new_relative("stale-source.jsonl")?,
        &session_id,
        &digest,
        "rebind-stale-route",
        &root_config,
        &current_route.model_ref,
    )?;
    let destination_records = JsonlSessionStore::read_event_records(&output.destination_path)?;
    let rebound_route = destination_records
        .iter()
        .find_map(|record| match record.session_log_entry().ok().flatten() {
            Some(sigil_kernel::SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route: Some(route),
                ..
            })) => Some(route),
            _ => None,
        })
        .expect("forked session should contain an exact route");

    assert_eq!(rebound_route, current_route);
    assert_ne!(
        rebound_route.semantic_fingerprint,
        stale_route.semantic_fingerprint
    );
    Ok(())
}

fn deepseek_root_config(directory: &Path) -> Result<RootConfig> {
    let path = directory.join("sigil.toml");
    fs::write(
        &path,
        r#"
[agent]
provider = "deepseek"
model = "deepseek-v4-flash"

[providers.deepseek]
base_url = "https://api.deepseek.com"
api_key = ""
"#,
    )?;
    RootConfig::load(&path)
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
    let root_config = deepseek_root_config(temp.path())?;
    let (_, current_route) =
        crate::provider_connections::resolve_default_model_route(&root_config)?;

    assert!(
        service
            .fork_session_at_turn(
                &session_ref,
                &session_id,
                "stale",
                "command-42",
                &root_config,
                &current_route.model_ref,
            )
            .is_err()
    );
    let output = service.fork_session_at_turn(
        &session_ref,
        &session_id,
        &digest,
        "command-42",
        &root_config,
        &current_route.model_ref,
    )?;
    let replay = service.fork_session_at_turn(
        &session_ref,
        &session_id,
        &digest,
        "command-42",
        &root_config,
        &current_route.model_ref,
    )?;

    assert_eq!(fs::read(&source)?, before);
    assert!(output.destination_path.exists());
    assert_eq!(output.copied_message_count, 2);
    assert_ne!(output.destination_session_id, session_id);
    assert_eq!(replay.destination_session_id, output.destination_session_id);
    let destination_records = JsonlSessionStore::read_event_records(&output.destination_path)?;
    assert!(destination_records.iter().any(|record| {
        matches!(
            record.session_log_entry().ok().flatten(),
            Some(sigil_kernel::SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route: Some(route),
                ..
            })) if route.model_ref.model_id == "deepseek-v4-flash"
        )
    }));
    Ok(())
}
