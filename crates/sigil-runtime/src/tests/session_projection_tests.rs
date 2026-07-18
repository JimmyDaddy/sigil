use std::{
    fs,
    time::{Duration, Instant},
};

use anyhow::Result;
use rusqlite::Connection;
use sigil_kernel::{
    AssistantMessageKind, ControlEntry, ModelMessage, Session, SessionLogEntry, SessionRef, TaskId,
    TaskRunEntry, TaskRunStatus,
};

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
fn session_projection_safe_projects_and_byte_bounds_title_and_task_objective() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let path = lifecycle.session_dir.join("safe.jsonl");
    let unsafe_text = format!(
        "{} https://example.com/private?token=secret-token",
        "界".repeat(100)
    );
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("deepseek", "chat").with_store(store);
    session.append_user_message(ModelMessage::user(&unsafe_text))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: TaskId::new("task-safe")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: unsafe_text.clone(),
        status: TaskRunStatus::Running,
        reason: None,
    }))?;

    projection.rebuild()?;
    let row = projection
        .list_workspace_entries()?
        .into_iter()
        .next()
        .expect("safe row");
    let title = row.title.expect("title");
    let objective = row.latest_task.expect("task").objective;

    assert!(title.len() <= SESSION_CATALOG_TITLE_MAX_BYTES);
    assert!(objective.len() <= SESSION_CATALOG_TITLE_MAX_BYTES);
    assert!(!title.contains("secret-token"));
    assert!(!objective.contains("secret-token"));
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
fn explicit_recovery_quarantines_projection_and_rebuilds_under_exclusive_lease() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Recover catalog",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    let mut before = projection.list_workspace_entries()?;

    let report = projection.quarantine_global_catalog_and_rebuild_workspace()?;
    let mut after = projection.list_workspace_entries()?;
    for row in before.iter_mut().chain(after.iter_mut()) {
        row.indexed_at_unix_ms = 0;
    }

    assert_eq!(before, after);
    assert_eq!(report.generation, 1);
    assert_eq!(report.invalidated_workspace_count, Some(1));
    assert_eq!(report.rebuilt_source_count, 1);
    assert!(report.quarantined_file_count >= 1);
    let quarantine_name = report
        .quarantine_directory_name
        .expect("existing projection should have a quarantine directory");
    let quarantine = projection
        .database_path()
        .parent()
        .expect("database parent")
        .join(quarantine_name);
    assert!(quarantine.is_dir());
    assert!(
        quarantine
            .join(
                projection
                    .database_path()
                    .file_name()
                    .expect("database filename")
            )
            .is_file()
    );
    Ok(())
}

#[test]
fn explicit_recovery_fails_closed_while_a_catalog_connection_is_active() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    projection.rebuild()?;
    let connection = projection.open_connection()?;

    let error = projection
        .quarantine_global_catalog_and_rebuild_workspace()
        .expect_err("active usage lease must block recovery");

    assert!(matches!(error, SessionCatalogProjectionError::RecoveryBusy));
    assert!(projection.database_path().is_file());
    drop(connection);
    projection.quarantine_global_catalog_and_rebuild_workspace()?;
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
fn global_projection_recovery_reports_cache_invalidation_and_sources_reconcile_again() -> Result<()>
{
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

    let report = first_projection.quarantine_global_catalog_and_rebuild_workspace()?;

    assert_eq!(report.invalidated_workspace_count, Some(2));
    assert_eq!(first_projection.list_workspace_entries()?.len(), 1);
    assert!(second_projection.list_workspace_entries()?.is_empty());
    second_projection.reconcile()?;
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
fn stable_degraded_sources_reuse_generation_across_reconciled_pages() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    for index in 0..3 {
        finalized_session(
            &lifecycle.session_dir.join(format!("ready-{index}.jsonl")),
            &format!("Ready {index}"),
            "deepseek",
            "chat",
        )?;
    }
    fs::write(lifecycle.session_dir.join("invalid.jsonl"), "not-json\n")?;
    fs::write(
        lifecycle.session_dir.join("legacy.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("legacy")))?
        ),
    )?;
    projection.rebuild()?;
    let unchanged = projection.reconcile()?;
    assert!(!unchanged.generation_changed);
    assert_eq!(unchanged.reused_source_count, 5);

    let first = projection.reconcile_and_query(SessionCatalogProjectionQuery {
        limit: 2,
        ..SessionCatalogProjectionQuery::default()
    })?;
    let second = projection.reconcile_and_query(SessionCatalogProjectionQuery {
        limit: 2,
        cursor: first.next_cursor.clone(),
        ..SessionCatalogProjectionQuery::default()
    })?;

    assert_eq!(first.generation, second.generation);
    assert_eq!(first.entries.len(), 2);
    assert_eq!(second.entries.len(), 2);
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
fn projection_rejects_unowned_default_pragma_database() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(
        projection
            .database_path()
            .parent()
            .expect("database parent"),
    )?;
    let connection = Connection::open(projection.database_path())?;
    connection.execute("CREATE TABLE unrelated(value TEXT)", [])?;
    drop(connection);

    let error = projection
        .rebuild()
        .expect_err("unowned SQLite database must fail closed");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::IncompatibleSchema {
            application_id: 0,
            user_version: 0,
        }
    ));
    let connection = Connection::open(projection.database_path())?;
    let value: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name = 'unrelated'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(value, 1);
    Ok(())
}

#[test]
fn projection_rejects_corrupt_sqlite_without_replacing_it() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(
        projection
            .database_path()
            .parent()
            .expect("database parent"),
    )?;
    fs::write(projection.database_path(), b"not a sqlite database")?;

    assert!(projection.rebuild().is_err());
    assert_eq!(
        fs::read(projection.database_path())?,
        b"not a sqlite database"
    );
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
fn projection_counts_duplicate_session_identity_without_overwriting_rows() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let first = lifecycle.session_dir.join("first.jsonl");
    let second = lifecycle.session_dir.join("second.jsonl");
    finalized_session(&first, "Duplicate", "deepseek", "chat")?;
    fs::copy(&first, &second)?;

    let report = projection.rebuild()?;

    assert_eq!(report.identity_conflict_count, 1);
    assert_eq!(projection.list_workspace_entries()?.len(), 2);
    Ok(())
}

#[test]
fn projection_materializes_oversize_and_scan_budget_states_without_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    fs::write(
        lifecycle.session_dir.join("large.jsonl"),
        "sensitive".repeat(32),
    )?;
    fs::write(lifecycle.session_dir.join("budget.jsonl"), "private")?;
    let limited = SessionCatalogProjectionService::new(
        lifecycle.with_limits(LocalSessionLifecycleLimits {
            max_stream_bytes: 128,
            max_total_validation_bytes: 0,
            ..LocalSessionLifecycleLimits::default()
        }),
        projection.database_path().to_path_buf(),
    );

    limited.rebuild()?;
    let rows = limited.list_workspace_entries()?;

    assert!(rows.iter().any(|row| {
        row.session_ref == "large.jsonl" && row.source_state == LocalSessionCatalogState::Oversized
    }));
    assert!(rows.iter().any(|row| {
        row.session_ref == "budget.jsonl"
            && row.source_state == LocalSessionCatalogState::ScanBudgetExceeded
    }));
    assert!(rows.iter().all(|row| row.title.is_none()));
    Ok(())
}

#[test]
fn projection_classifies_gap_checksum_and_mixed_identity_streams_as_invalid() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let seed_a = temp.path().join("seed-a.jsonl");
    let seed_b = temp.path().join("seed-b.jsonl");
    finalized_session(&seed_a, "Checksum target", "deepseek", "chat")?;
    finalized_session(&seed_b, "Other identity", "anthropic", "claude")?;
    let a_lines = fs::read_to_string(&seed_a)?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let b_lines = fs::read_to_string(&seed_b)?
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    fs::write(
        lifecycle.session_dir.join("gap.jsonl"),
        format!("{}\n{}\n", a_lines[0], a_lines[2]),
    )?;
    fs::write(
        lifecycle.session_dir.join("checksum.jsonl"),
        format!(
            "{}\n",
            a_lines
                .join("\n")
                .replace("Checksum target", "Checksum forged")
        ),
    )?;
    fs::write(
        lifecycle.session_dir.join("mixed.jsonl"),
        format!("{}\n{}\n", a_lines[0], b_lines[1]),
    )?;

    let report = projection.rebuild()?;
    let rows = projection.list_workspace_entries()?;

    assert_eq!(report.degraded_source_count, 3);
    assert!(
        rows.iter()
            .all(|row| row.source_state == LocalSessionCatalogState::Invalid)
    );
    assert!(rows.iter().all(|row| row.session_id.is_none()));
    assert!(rows.iter().all(|row| row.title.is_none()));
    Ok(())
}

#[test]
fn truncated_reconcile_preserves_unscanned_rows_until_a_complete_scan() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    for name in ["first", "second", "third"] {
        finalized_session(
            &lifecycle.session_dir.join(format!("{name}.jsonl")),
            name,
            "deepseek",
            "chat",
        )?;
    }
    projection.rebuild()?;
    let limited_lifecycle = lifecycle.clone().with_limits(LocalSessionLifecycleLimits {
        max_catalog_entries: 1,
        ..LocalSessionLifecycleLimits::default()
    });
    let limited_projection = SessionCatalogProjectionService::new(
        limited_lifecycle,
        projection.database_path().to_path_buf(),
    );
    fs::remove_file(lifecycle.session_dir.join("third.jsonl"))?;

    let partial = limited_projection.reconcile()?;

    assert_eq!(partial.scanned_source_count, 1);
    assert_eq!(partial.truncated_source_count, 1);
    assert_eq!(partial.removed_source_count, 0);
    assert_eq!(limited_projection.list_workspace_entries()?.len(), 3);

    let complete = projection.reconcile()?;
    assert_eq!(complete.truncated_source_count, 0);
    assert_eq!(complete.removed_source_count, 1);
    assert_eq!(projection.list_workspace_entries()?.len(), 2);
    Ok(())
}

#[test]
fn sqlite_writer_contention_returns_within_the_bounded_busy_timeout() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    projection.rebuild()?;
    let writer = Connection::open(projection.database_path())?;
    writer.execute_batch("BEGIN IMMEDIATE")?;
    let started = Instant::now();

    let error = projection
        .reconcile()
        .expect_err("concurrent writer must not wait indefinitely");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::Sqlite { .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(5));
    writer.execute_batch("ROLLBACK")?;
    Ok(())
}

#[test]
fn concurrent_first_page_reads_and_reconcile_remain_bounded() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("first.jsonl"),
        "Concurrent read",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let reader_projection = projection.clone();
    let reader_barrier = std::sync::Arc::clone(&barrier);
    let reader = std::thread::spawn(move || -> Result<()> {
        reader_barrier.wait();
        for _ in 0..20 {
            let page = reader_projection.query(SessionCatalogProjectionQuery {
                limit: 1,
                ..SessionCatalogProjectionQuery::default()
            })?;
            assert_eq!(page.entries.len(), 1);
        }
        Ok(())
    });
    barrier.wait();
    finalized_session(
        &lifecycle.session_dir.join("second.jsonl"),
        "Concurrent write",
        "deepseek",
        "chat",
    )?;
    projection.reconcile()?;

    reader.join().expect("reader thread should join")?;
    Ok(())
}

#[test]
fn source_metadata_drift_is_rejected_before_projection_publish() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    fs::create_dir_all(&sessions)?;
    let path = sessions.join("session.jsonl");
    finalized_session(&path, "Drift", "deepseek", "chat")?;
    let mut candidates = direct_jsonl_candidates(&fs::canonicalize(&sessions)?)?;
    let candidate = candidates.pop().expect("session candidate");
    fs::write(&path, "changed after scan")?;

    let error = ensure_source_stable(&candidate).expect_err("source drift must fail closed");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::Source { .. }
    ));
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
fn reconcile_and_query_rejects_invalid_input_before_database_creation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");

    let error = projection
        .reconcile_and_query(SessionCatalogProjectionQuery {
            limit: 0,
            ..SessionCatalogProjectionQuery::default()
        })
        .expect_err("invalid query must fail before reconciliation");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::InvalidQuery { .. }
    ));
    assert!(!projection.database_path().exists());
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

#[cfg(unix)]
#[test]
fn projection_rejects_broken_session_directory_symlink_without_deleting_rows() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Keep projected row",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    fs::remove_dir_all(&lifecycle.session_dir)?;
    symlink(temp.path().join("missing-sessions"), &lifecycle.session_dir)?;

    let error = projection
        .reconcile()
        .expect_err("broken directory symlink must fail closed");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::Source { .. }
    ));
    assert_eq!(projection.list_workspace_entries()?.len(), 1);
    Ok(())
}

#[cfg(unix)]
#[test]
fn projection_materializes_source_symlink_as_invalid_without_following_content() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let outside = temp.path().join("outside.jsonl");
    finalized_session(&outside, "Must not follow", "deepseek", "chat")?;
    symlink(&outside, lifecycle.session_dir.join("linked.jsonl"))?;

    projection.rebuild()?;
    let row = projection
        .list_workspace_entries()?
        .into_iter()
        .next()
        .expect("symlink row");

    assert_eq!(row.source_state, LocalSessionCatalogState::Invalid);
    assert!(row.session_id.is_none());
    assert!(row.title.is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn projection_creates_private_database_parent_and_sqlite_files() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    projection.rebuild()?;

    let parent_mode = fs::metadata(
        projection
            .database_path()
            .parent()
            .expect("database parent"),
    )?
    .permissions()
    .mode()
        & 0o777;
    assert_eq!(parent_mode, 0o700);
    for path in sqlite_projection_files(projection.database_path()) {
        if path.exists() {
            assert_eq!(fs::metadata(path)?.permissions().mode() & 0o777, 0o600);
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn projection_rejects_broken_symlink_recovery_lease_without_creating_target() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (_lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(
        projection
            .database_path()
            .parent()
            .expect("database parent"),
    )?;
    let outside = temp.path().join("outside-recovery-lock");
    symlink(
        &outside,
        sqlite_sidecar_path(projection.database_path(), ".recovery-lock"),
    )?;

    let error = projection
        .rebuild()
        .expect_err("symlink recovery lease must fail closed");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::UnsafePath { .. }
    ));
    assert!(!outside.exists());
    Ok(())
}
