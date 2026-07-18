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

#[test]
fn incremental_reconcile_reuses_unchanged_rows_and_tracks_pin_append_delete() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let first = lifecycle.session_dir.join("first.jsonl");
    let second = lifecycle.session_dir.join("second.jsonl");
    finalized_session(&first, "First", "deepseek", "chat")?;
    finalized_session(&second, "Second", "anthropic", "claude")?;
    let initial = projection.rebuild()?;

    let unchanged = projection.reconcile()?;

    assert_eq!(unchanged.generation, initial.generation);
    assert!(!unchanged.generation_changed);
    assert_eq!(unchanged.reused_source_count, 2);
    assert_eq!(unchanged.updated_source_count, 0);
    lifecycle.set_session_pin(&first, true, 200)?;
    let pinned = projection.reconcile()?;
    assert!(pinned.generation_changed);
    assert_eq!(pinned.generation, initial.generation + 1);
    assert_eq!(pinned.reused_source_count, 2);
    assert_eq!(pinned.updated_source_count, 1);
    JsonlSessionStore::new(&first)?.append(&SessionLogEntry::User(ModelMessage::user("Later")))?;
    let appended = projection.reconcile()?;
    assert_eq!(appended.reused_source_count, 1);
    assert_eq!(appended.updated_source_count, 1);
    let first_row = projection
        .list_workspace_entries()?
        .into_iter()
        .find(|entry| entry.session_ref == "first.jsonl")
        .expect("first row");
    assert_eq!(first_row.user_message_count, 2);
    fs::remove_file(second)?;
    let removed = projection.reconcile()?;
    assert_eq!(removed.removed_source_count, 1);
    assert_eq!(projection.list_workspace_entries()?.len(), 1);
    Ok(())
}

#[test]
fn catalog_query_paginates_without_duplicates_and_binds_cursor_to_filters() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    for index in 0..5 {
        finalized_session(
            &lifecycle.session_dir.join(format!("session-{index}.jsonl")),
            &format!("Prompt {index}"),
            if index % 2 == 0 {
                "deepseek"
            } else {
                "anthropic"
            },
            "model",
        )?;
    }
    projection.rebuild()?;
    let full = projection.query(SessionCatalogProjectionQuery {
        limit: 100,
        ..SessionCatalogProjectionQuery::default()
    })?;
    let first = projection.query(SessionCatalogProjectionQuery {
        limit: 2,
        ..SessionCatalogProjectionQuery::default()
    })?;
    let second = projection.query(SessionCatalogProjectionQuery {
        limit: 2,
        cursor: first.next_cursor.clone(),
        ..SessionCatalogProjectionQuery::default()
    })?;
    let third = projection.query(SessionCatalogProjectionQuery {
        limit: 2,
        cursor: second.next_cursor.clone(),
        ..SessionCatalogProjectionQuery::default()
    })?;
    let paged = first
        .entries
        .iter()
        .chain(&second.entries)
        .chain(&third.entries)
        .map(|entry| entry.session_ref.clone())
        .collect::<Vec<_>>();

    assert_eq!(paged.len(), 5);
    assert_eq!(
        paged,
        full.entries
            .iter()
            .map(|entry| entry.session_ref.clone())
            .collect::<Vec<_>>()
    );
    assert!(third.next_cursor.is_none());
    let error = projection
        .query(SessionCatalogProjectionQuery {
            limit: 2,
            cursor: first.next_cursor,
            provider_name: Some("deepseek".to_owned()),
            ..SessionCatalogProjectionQuery::default()
        })
        .expect_err("cursor must be bound to filters");
    assert!(matches!(
        error,
        SessionCatalogProjectionError::InvalidCursor { .. }
    ));
    Ok(())
}

#[test]
fn catalog_query_filters_literal_search_provider_pin_and_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let literal = lifecycle.session_dir.join("literal.jsonl");
    finalized_session(&literal, "Find 100%_Literal", "deepseek", "deepseek-v4")?;
    finalized_session(
        &lifecycle.session_dir.join("other.jsonl"),
        "Other title",
        "anthropic",
        "claude",
    )?;
    fs::write(lifecycle.session_dir.join("invalid.jsonl"), "invalid\n")?;
    lifecycle.set_session_pin(&literal, true, 300)?;
    projection.rebuild()?;

    let matched = projection.query(SessionCatalogProjectionQuery {
        search: Some("%_literal".to_owned()),
        provider_name: Some("deepseek".to_owned()),
        pinned: Some(true),
        source_state: Some(LocalSessionCatalogState::Ready),
        ..SessionCatalogProjectionQuery::default()
    })?;
    let invalid = projection.query(SessionCatalogProjectionQuery {
        source_state: Some(LocalSessionCatalogState::Invalid),
        ..SessionCatalogProjectionQuery::default()
    })?;

    assert_eq!(matched.entries.len(), 1);
    assert_eq!(matched.entries[0].session_ref, "literal.jsonl");
    assert_eq!(invalid.entries.len(), 1);
    assert_eq!(invalid.entries[0].session_ref, "invalid.jsonl");
    Ok(())
}

#[test]
fn catalog_query_rejects_stale_generation_cursor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    for index in 0..3 {
        finalized_session(
            &lifecycle.session_dir.join(format!("session-{index}.jsonl")),
            &format!("Prompt {index}"),
            "deepseek",
            "chat",
        )?;
    }
    projection.rebuild()?;
    let first = projection.query(SessionCatalogProjectionQuery {
        limit: 1,
        ..SessionCatalogProjectionQuery::default()
    })?;
    finalized_session(
        &lifecycle.session_dir.join("new.jsonl"),
        "New session",
        "deepseek",
        "chat",
    )?;
    projection.reconcile()?;

    let error = projection
        .query(SessionCatalogProjectionQuery {
            limit: 1,
            cursor: first.next_cursor,
            ..SessionCatalogProjectionQuery::default()
        })
        .expect_err("changed generation must invalidate cursor");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::StaleCursor { .. }
    ));
    Ok(())
}

#[test]
fn catalog_query_rejects_unbounded_blank_and_malformed_inputs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");

    for query in [
        SessionCatalogProjectionQuery {
            limit: 0,
            ..SessionCatalogProjectionQuery::default()
        },
        SessionCatalogProjectionQuery {
            limit: MAX_SESSION_CATALOG_PAGE_SIZE + 1,
            ..SessionCatalogProjectionQuery::default()
        },
        SessionCatalogProjectionQuery {
            search: Some("   ".to_owned()),
            ..SessionCatalogProjectionQuery::default()
        },
    ] {
        assert!(matches!(
            projection
                .query(query)
                .expect_err("invalid query must fail closed"),
            SessionCatalogProjectionError::InvalidQuery { .. }
        ));
    }
    assert!(matches!(
        projection
            .query(SessionCatalogProjectionQuery {
                cursor: Some("not-base64!".to_owned()),
                ..SessionCatalogProjectionQuery::default()
            })
            .expect_err("malformed cursor must fail closed"),
        SessionCatalogProjectionError::InvalidCursor { .. }
    ));
    Ok(())
}

#[test]
fn generation_compare_and_swap_rejects_an_older_scan() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Generation",
        "deepseek",
        "chat",
    )?;
    let initial = projection.rebuild()?;
    let mut stale_connection = projection.open_connection()?;
    projection.rebuild()?;
    let transaction = stale_connection.transaction_with_behavior(TransactionBehavior::Immediate)?;

    assert!(!workspace_generation_matches(
        &transaction,
        "workspace-1",
        Some(initial.generation),
    )?);
    transaction.rollback()?;
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
