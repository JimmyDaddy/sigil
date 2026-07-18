use std::fs;

use anyhow::Result;
use rusqlite::Connection;
use sigil_kernel::{AssistantMessageKind, ControlEntry, ModelMessage, Session, SessionLogEntry};

use super::*;

fn finalized_session(path: &Path, prompt: &str, provider: &str, model: &str) -> Result<String> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::new(provider, model).with_store(store);
    session.append_control(ControlEntry::SessionIdentity {
        provider_name: provider.to_owned(),
        model_name: model.to_owned(),
    })?;
    session.append_user_message(ModelMessage::user(prompt))?;
    session.append_assistant_message(ModelMessage::assistant_with_kind(
        Some("done".to_owned()),
        Vec::new(),
        AssistantMessageKind::FinalAnswer,
    ))?;
    Ok(JsonlSessionStore::read_event_records(path)?
        .first()
        .expect("session has durable records")
        .session_id()
        .to_owned())
}

fn projection_service(
    root: &Path,
    workspace_id: &str,
) -> (
    LocalSessionLifecycleService,
    SessionCatalogProjectionService,
) {
    let workspace = root.join(workspace_id);
    let lifecycle = LocalSessionLifecycleService::new(
        workspace_id,
        workspace.join("sessions"),
        workspace.join("session-exports"),
    );
    let projection = SessionCatalogProjectionService::new(
        lifecycle.clone(),
        root.join("projections/session-catalog-v1.sqlite3"),
    );
    (lifecycle, projection)
}

#[test]
fn session_projection_rebuilds_safe_rows_and_pin_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let first = lifecycle.session_dir.join("first.jsonl");
    let second = lifecycle.session_dir.join("second.jsonl");
    let first_id = finalized_session(&first, "First private prompt", "deepseek", "chat")?;
    finalized_session(&second, "Second prompt", "anthropic", "claude")?;
    lifecycle.set_session_pin(&first, true, 100)?;

    let report = projection.rebuild()?;
    let rows = projection.list_workspace_entries()?;

    assert_eq!(report.generation, 1);
    assert_eq!(report.indexed_source_count, 2);
    assert_eq!(report.degraded_source_count, 0);
    assert_eq!(rows.len(), 2);
    let first = rows
        .iter()
        .find(|row| row.session_ref == "first.jsonl")
        .expect("first row");
    assert_eq!(first.session_id.as_deref(), Some(first_id.as_str()));
    assert_eq!(first.provider_name.as_deref(), Some("deepseek"));
    assert_eq!(first.title.as_deref(), Some("First private prompt"));
    assert_eq!(first.user_message_count, 1);
    assert_eq!(first.assistant_message_count, 1);
    assert!(first.pinned);
    assert_eq!(
        first.source_content_sha256.as_deref().map(str::len),
        Some(64)
    );
    assert!(first.last_record_checksum.is_some());
    Ok(())
}

#[test]
fn deleting_projection_and_rebuilding_produces_equivalent_rows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Rebuild me",
        "deepseek",
        "chat",
    )?;

    projection.rebuild()?;
    let mut before = projection.list_workspace_entries()?;
    fs::remove_file(projection.database_path())?;
    let _ = fs::remove_file(format!("{}-wal", projection.database_path().display()));
    let _ = fs::remove_file(format!("{}-shm", projection.database_path().display()));
    projection.rebuild()?;
    let mut after = projection.list_workspace_entries()?;
    for row in before.iter_mut().chain(after.iter_mut()) {
        row.indexed_at_unix_ms = 0;
    }

    assert_eq!(before, after);
    Ok(())
}

#[test]
fn global_projection_rebuild_preserves_other_workspace_rows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (first_lifecycle, first_projection) = projection_service(temp.path(), "workspace-1");
    let (second_lifecycle, second_projection) = projection_service(temp.path(), "workspace-2");
    fs::create_dir_all(&first_lifecycle.session_dir)?;
    fs::create_dir_all(&second_lifecycle.session_dir)?;
    finalized_session(
        &first_lifecycle.session_dir.join("first.jsonl"),
        "First workspace",
        "deepseek",
        "chat",
    )?;
    finalized_session(
        &second_lifecycle.session_dir.join("second.jsonl"),
        "Second workspace",
        "anthropic",
        "claude",
    )?;

    first_projection.rebuild()?;
    second_projection.rebuild()?;
    first_projection.rebuild()?;

    assert_eq!(first_projection.list_workspace_entries()?.len(), 1);
    assert_eq!(second_projection.list_workspace_entries()?.len(), 1);
    Ok(())
}

#[test]
fn projection_preserves_invalid_and_legacy_sources_without_message_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    fs::write(lifecycle.session_dir.join("invalid.jsonl"), "not-json\n")?;
    fs::write(
        lifecycle.session_dir.join("legacy.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user(
                "legacy body must not be copied",
            )))?
        ),
    )?;

    let report = projection.rebuild()?;
    let rows = projection.list_workspace_entries()?;
    let database_bytes = fs::read(projection.database_path())?;

    assert_eq!(report.degraded_source_count, 2);
    assert!(rows.iter().all(|row| row.title.is_none()));
    assert!(rows.iter().all(|row| row.session_id.is_none()));
    assert!(
        !database_bytes
            .windows("legacy body must not be copied".len())
            .any(|window| window == b"legacy body must not be copied")
    );
    Ok(())
}

#[test]
fn projection_rejects_incompatible_schema_without_deleting_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(
        projection
            .database_path()
            .parent()
            .expect("database parent"),
    )?;
    let connection = Connection::open(projection.database_path())?;
    connection.pragma_update(None, "application_id", 42)?;
    connection.pragma_update(None, "user_version", 9)?;
    drop(connection);

    let error = projection
        .rebuild()
        .expect_err("incompatible schema must fail closed");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::IncompatibleSchema {
            application_id: 42,
            user_version: 9,
        }
    ));
    assert!(projection.database_path().exists());
    Ok(())
}

#[test]
fn projection_rejects_unknown_persisted_source_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "State validation",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    let connection = Connection::open(projection.database_path())?;
    connection.execute(
        "UPDATE session_catalog_entry_v1 SET source_state = 'future_state'",
        [],
    )?;
    drop(connection);

    let error = projection
        .list_workspace_entries()
        .expect_err("unknown state must not be coerced");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::Sqlite { .. }
    ));
    Ok(())
}

#[cfg(unix)]
#[test]
fn projection_rejects_symlink_database_path() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(
        projection
            .database_path()
            .parent()
            .expect("database parent"),
    )?;
    let target = temp.path().join("outside.sqlite3");
    fs::write(&target, "outside")?;
    symlink(&target, projection.database_path())?;

    let error = projection
        .rebuild()
        .expect_err("symlink database must fail closed");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::UnsafePath { .. }
    ));
    assert_eq!(fs::read_to_string(target)?, "outside");
    Ok(())
}
