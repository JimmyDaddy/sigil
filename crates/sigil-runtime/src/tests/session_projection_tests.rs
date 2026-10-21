use std::{
    fs,
    time::{Duration, Instant},
};

use anyhow::Result;
use fs2::FileExt;
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
        resolved_model_route: None,
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

fn explicit_plan_session(path: &Path, objective: &str) -> Result<String> {
    let store = JsonlSessionStore::new(path)?;
    let mut session = Session::load_from_store("custom", "model", store)?;
    let turn = sigil_kernel::ConversationTurnRef::new(
        session.session_scope_id(),
        "explicit-plan-source",
        "explicit-plan-run",
    )?;
    let review_id = sigil_kernel::plan_review_id_for_source(&turn);
    let attempt_id = sigil_kernel::plan_review_attempt_id_for_review(&review_id);
    session.append_control(ControlEntry::PlanReviewAttempt(
        sigil_kernel::PlanReviewAttemptEntry {
            plan_id: sigil_kernel::plan_review_plan_id_for_attempt(&review_id, &attempt_id),
            child_session_ref: sigil_kernel::plan_review_child_session_ref(&review_id, &attempt_id),
            plan_review_id: review_id,
            attempt_id,
            source: sigil_kernel::PlanReviewSource::ExplicitPlanCommand,
            source_turn: turn,
            explicit_objective: Some(objective.to_owned()),
            route_decision_id: None,
            revision_request_id: None,
            attempt_ordinal: 1,
            base_plan_id: None,
            base_plan_hash: None,
            workspace_snapshot_id: None,
            pending_user_input: None,
            status: sigil_kernel::PlanReviewAttemptStatus::Started,
            terminal_reason: None,
            recorded_at_ms: 1,
        },
    ))?;
    Ok(session.session_scope_id().to_owned())
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
fn catalog_explicit_plan_title_is_searchable_without_a_user_message() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "plan-search");
    let source = lifecycle.session_dir.join("agenda.jsonl");
    let session_id = explicit_plan_session(&source, "为活动组织者准备议程工具")?;
    let before = fs::read(&source)?;
    projection.reconcile()?;
    let page = projection.query(SessionCatalogProjectionQuery {
        search: Some("议程".to_owned()),
        ..SessionCatalogProjectionQuery::default()
    })?;

    assert_eq!(page.entries.len(), 1);
    let row = &page.entries[0];
    assert_eq!(row.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(row.title.as_deref(), Some("为活动组织者准备议程工具"));
    assert_eq!(row.user_message_count, 0);
    assert_eq!(row.source_state, LocalSessionCatalogState::Ready);
    assert_eq!(fs::read(&source)?, before);
    Ok(())
}

#[test]
fn catalog_explicit_plan_title_preserves_manual_and_generated_title_priority() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "plan-title-priority");
    let manual = lifecycle.session_dir.join("manual.jsonl");
    let generated = lifecycle.session_dir.join("generated.jsonl");
    let manual_id = explicit_plan_session(&manual, "后备议程目标")?;
    let generated_id = explicit_plan_session(&generated, "另一后备议程目标")?;
    let manual_before = fs::read(&manual)?;
    let generated_before = fs::read(&generated)?;
    lifecycle.rename_session(
        &SessionRef::new_relative("manual.jsonl")?,
        &manual_id,
        "人工指定标题",
        2,
    )?;
    for (session_ref, session_id) in [
        ("manual.jsonl", manual_id.as_str()),
        ("generated.jsonl", generated_id.as_str()),
    ] {
        lifecycle.record_generated_title(
            &SessionRef::new_relative(session_ref)?,
            session_id,
            "系统生成标题",
            "custom",
            "model",
            None,
            None,
            3,
        )?;
    }
    projection.reconcile()?;
    let rows = projection.list_workspace_entries()?;
    assert_eq!(
        rows.iter()
            .find(|row| row.session_ref == "manual.jsonl")
            .and_then(|row| row.title.as_deref()),
        Some("人工指定标题")
    );
    assert_eq!(
        rows.iter()
            .find(|row| row.session_ref == "generated.jsonl")
            .and_then(|row| row.title.as_deref()),
        Some("系统生成标题")
    );
    assert!(rows.iter().all(|row| row.user_message_count == 0));
    assert_eq!(fs::read(&manual)?, manual_before);
    assert_eq!(fs::read(&generated)?, generated_before);
    Ok(())
}

#[test]
fn catalog_explicit_plan_title_revision_reprojects_unchanged_null_titles_per_workspace()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (first_lifecycle, first) = projection_service(temp.path(), "first-plan-workspace");
    let (second_lifecycle, second) = projection_service(temp.path(), "second-plan-workspace");
    let first_source = first_lifecycle.session_dir.join("same-name.jsonl");
    let second_source = second_lifecycle.session_dir.join("same-name.jsonl");
    explicit_plan_session(&first_source, "第一工作区议程")?;
    explicit_plan_session(&second_source, "第二工作区议程")?;
    first.reconcile()?;
    second.reconcile()?;
    let first_bytes = fs::read(&first_source)?;
    let second_bytes = fs::read(&second_source)?;
    let first_modified = fs::metadata(&first_source)?.modified()?;
    let second_modified = fs::metadata(&second_source)?.modified()?;
    let connection = Connection::open(first.database_path())?;
    connection.execute(
        "UPDATE session_catalog_entry_v1 SET title = NULL, title_search = NULL",
        [],
    )?;
    connection.execute(
        "UPDATE session_catalog_workspace_v1 SET projection_revision = ?1",
        [SESSION_CATALOG_PROJECTION_REVISION - 1],
    )?;
    drop(connection);
    let query = || SessionCatalogProjectionQuery {
        search: Some("议程".to_owned()),
        ..SessionCatalogProjectionQuery::default()
    };
    assert!(first.query(query())?.entries.is_empty());
    let rebuilt = first.reconcile()?;
    assert!(rebuilt.generation_changed);
    assert_eq!(rebuilt.reused_source_count, 0);
    assert_eq!(rebuilt.updated_source_count, 1);
    assert_eq!(
        first.query(query())?.entries[0].title.as_deref(),
        Some("第一工作区议程")
    );
    assert_eq!(second.list_workspace_entries()?[0].title, None);
    let untouched_revision: u16 = Connection::open(first.database_path())?.query_row(
        "SELECT projection_revision FROM session_catalog_workspace_v1 WHERE workspace_id = ?1",
        [&second_lifecycle.workspace_id],
        |row| row.get(0),
    )?;
    assert_eq!(untouched_revision, SESSION_CATALOG_PROJECTION_REVISION - 1);
    let unchanged = first.reconcile()?;
    assert!(!unchanged.generation_changed);
    assert_eq!(unchanged.reused_source_count, 1);
    second.reconcile()?;
    assert_eq!(
        second.query(query())?.entries[0].title.as_deref(),
        Some("第二工作区议程")
    );
    assert!(
        first
            .query(SessionCatalogProjectionQuery {
                search: Some("第二工作区".to_owned()),
                ..SessionCatalogProjectionQuery::default()
            })?
            .entries
            .is_empty()
    );
    assert_eq!(fs::read(&first_source)?, first_bytes);
    assert_eq!(fs::read(&second_source)?, second_bytes);
    assert_eq!(fs::metadata(&first_source)?.modified()?, first_modified);
    assert_eq!(fs::metadata(&second_source)?.modified()?, second_modified);
    Ok(())
}

#[test]
fn catalog_revalidates_cached_invalid_without_source_changes_or_replaying_ready_history()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "invalid-cache-recovery");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("healthy.jsonl");
    let other = lifecycle.session_dir.join("unchanged-ready.jsonl");
    let session_id = finalized_session(&source, "Healthy unchanged source", "custom", "model")?;
    finalized_session(&other, "Reuse this large history", "custom", "model")?;
    projection.rebuild()?;
    let expected = projection
        .list_workspace_entries()?
        .into_iter()
        .find(|row| row.session_ref == "healthy.jsonl")
        .expect("healthy row");
    let bytes = fs::read(&source)?;
    let modified = fs::metadata(&source)?.modified()?;
    // Reproduce the old cache state independently of the healthy journal. No source append,
    // timestamp change, or repair is available to make the fingerprint look different.
    Connection::open(projection.database_path())?.execute(
        "UPDATE session_catalog_entry_v1 SET source_state = 'invalid', source_diagnostic = 'invalid_event_stream', session_id = NULL, title = NULL, user_message_count = 0, assistant_message_count = 0, indexed_at_unix_ms = 0 WHERE workspace_id = ?1 AND session_ref = 'healthy.jsonl'",
        params![lifecycle.workspace_id],
    )?;
    // A ready row must still be reused: replaying all unchanged histories would hit this real
    // data-file writer lock and fail rather than recover only the poisoned entry.
    let unrelated_writer = OpenOptions::new().read(true).write(true).open(&other)?;
    FileExt::try_lock_exclusive(&unrelated_writer)?;
    let report = projection.reconcile()?;
    assert_eq!(report.reused_source_count, 1);
    assert_eq!(report.updated_source_count, 1);
    assert_eq!(report.degraded_source_count, 0);
    let restored = projection
        .list_workspace_entries()?
        .into_iter()
        .find(|row| row.session_ref == "healthy.jsonl")
        .expect("restored row");
    assert_eq!(restored.source_state, LocalSessionCatalogState::Ready);
    assert_eq!(restored.session_id.as_deref(), Some(session_id.as_str()));
    assert_eq!(restored.source_diagnostic, None);
    assert_eq!(restored.user_message_count, expected.user_message_count);
    assert_eq!(
        restored.assistant_message_count,
        expected.assistant_message_count
    );
    assert_eq!(restored.title, expected.title);
    assert_eq!(
        restored.source_content_sha256,
        expected.source_content_sha256
    );
    assert_eq!(fs::read(&source)?, bytes);
    assert_eq!(fs::metadata(&source)?.modified()?, modified);
    Ok(())
}

#[test]
fn catalog_reader_busy_preserves_last_valid_row_and_recovers_after_real_writer_releases()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "reader-busy");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("active.jsonl");
    finalized_session(&source, "Before writer", "custom", "model")?;
    projection.rebuild()?;
    let original = projection.list_workspace_entries()?;
    let store = JsonlSessionStore::new(&source)?;
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Committed before the contended read",
    )))?;
    let bytes = fs::read(&source)?;
    let writer = OpenOptions::new().read(true).write(true).open(&source)?;
    FileExt::try_lock_exclusive(&writer)?;
    let error = projection
        .reconcile()
        .expect_err("path-only observer must report external writer contention");
    let SessionCatalogProjectionError::SourceRead { source: error } = error else {
        panic!("contention must retain its typed source: {error}");
    };
    let busy = error
        .downcast_ref::<sigil_kernel::session::SessionIoBusyError>()
        .expect("actual reader lock failure");
    assert_eq!(busy.kind, sigil_kernel::session::SessionIoBusyKind::Reader);
    assert_eq!(
        projection.list_workspace_entries()?,
        original,
        "busy is not evidence of an invalid zero-message source"
    );
    assert_eq!(
        fs::read(&source)?,
        bytes,
        "the catalog must not repair the journal"
    );
    drop(writer);
    // The live owner can append after the observer has returned. Catalog snapshots retain no
    // writer lease or data-file shared lock, and the next bounded refresh sees the real write.
    store.append(&SessionLogEntry::User(ModelMessage::user(
        "Written after reader returned",
    )))?;
    projection.reconcile()?;
    let restored = projection.list_workspace_entries()?.remove(0);
    assert_eq!(restored.source_state, LocalSessionCatalogState::Ready);
    assert_eq!(restored.source_diagnostic, None);
    assert_eq!(
        restored.user_message_count,
        original[0].user_message_count + 2
    );
    Ok(())
}

#[test]
fn catalog_corrupt_source_has_bounded_retry_cooldown_and_preserves_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "invalid-cooldown");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("broken.jsonl");
    fs::write(&source, b"not a session stream\n")?;
    projection.rebuild()?;
    let invalid = projection.list_workspace_entries()?;
    let bytes = fs::read(&source)?;
    let writer = OpenOptions::new().read(true).write(true).open(&source)?;
    FileExt::try_lock_exclusive(&writer)?;
    let cooled = projection.reconcile()?;
    assert_eq!(
        cooled.reused_source_count, 1,
        "a poll during cooldown cannot re-read the locked corrupt source"
    );
    assert_eq!(cooled.updated_source_count, 0);
    Connection::open(projection.database_path())?.execute(
        "UPDATE session_catalog_entry_v1 SET indexed_at_unix_ms = 0 WHERE workspace_id = ?1",
        params![lifecycle.workspace_id],
    )?;
    let before_retry = projection.list_workspace_entries()?;
    assert!(
        matches!(
            projection.reconcile(),
            Err(SessionCatalogProjectionError::SourceRead { .. })
        ),
        "expired negative cache must actually retry the reader"
    );
    assert_eq!(projection.list_workspace_entries()?, before_retry);
    drop(writer);
    let retried = projection.reconcile()?;
    assert_eq!(retried.reused_source_count, 0);
    assert_eq!(retried.degraded_source_count, 1);
    let current = projection.list_workspace_entries()?.remove(0);
    assert_eq!(current.source_state, LocalSessionCatalogState::Invalid);
    assert_eq!(current.source_diagnostic, invalid[0].source_diagnostic);
    assert_eq!(fs::read(&source)?, bytes);
    Ok(())
}

#[test]
fn catalog_source_read_diagnostic_preserves_real_cancellation_and_io_causes() -> Result<()> {
    let budget = sigil_kernel::SessionReadBudget::default();
    budget.cancel();
    let cancelled =
        classify_source_read_error(budget.check().expect_err("actual cancelled budget"))
            .expect_err("cancel is not corruption");
    let SessionCatalogProjectionError::SourceRead { source } = cancelled else {
        panic!("typed cancelled source");
    };
    assert!(source.is::<sigil_kernel::SessionObservationCancelled>());
    let temp = tempfile::tempdir()?;
    let missing = File::open(temp.path().join("missing.jsonl")).expect_err("actual missing source");
    let io = classify_source_read_error(missing.into()).expect_err("I/O is not corruption");
    let SessionCatalogProjectionError::SourceRead { source } = io else {
        panic!("typed I/O source");
    };
    assert_eq!(
        source
            .downcast_ref::<std::io::Error>()
            .expect("I/O cause")
            .kind(),
        std::io::ErrorKind::NotFound
    );
    let (lifecycle, projection) = projection_service(temp.path(), "missing-observation");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let path = lifecycle.session_dir.join("removed.jsonl");
    finalized_session(&path, "Removed after discovery", "custom", "model")?;
    let candidate = lifecycle.session_source_candidates()?.remove(0);
    fs::remove_file(&path)?;
    let error = projection
        .project_candidate(
            candidate,
            LocalSessionCatalogState::Ready,
            &BTreeMap::new(),
            &BTreeMap::new(),
            current_unix_ms(),
        )
        .expect_err("disappeared candidate must not become missing-identity invalid");
    let SessionCatalogProjectionError::SourceRead { source } = error else {
        panic!("preserved disappearance I/O");
    };
    assert_eq!(
        source
            .downcast_ref::<std::io::Error>()
            .expect("source I/O")
            .kind(),
        std::io::ErrorKind::NotFound
    );
    Ok(())
}

#[test]
fn catalog_invalid_utf8_is_corruption_not_transient_io() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "invalid-utf8");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let path = lifecycle.session_dir.join("invalid.jsonl");
    let bytes = [0xff, 0xfe, b'\n'];
    fs::write(&path, bytes)?;
    assert_eq!(projection.rebuild()?.degraded_source_count, 1);
    let entry = projection.list_workspace_entries()?.remove(0);
    assert_eq!(entry.source_state, LocalSessionCatalogState::Invalid);
    assert_eq!(
        entry.source_diagnostic,
        Some(SessionCatalogSourceDiagnostic::InvalidEventStream)
    );
    assert_eq!(fs::read(&path)?, bytes);
    Ok(())
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
        title: None,

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
fn projection_preserves_invalid_sources_without_message_content() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    fs::write(lifecycle.session_dir.join("invalid.jsonl"), "not-json\n")?;
    fs::write(
        lifecycle.session_dir.join("invalid-session-entry.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user(
                "invalid body must not be copied",
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
            .windows("invalid body must not be copied".len())
            .any(|window| window == b"invalid body must not be copied")
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
        lifecycle.session_dir.join("invalid-session-entry.jsonl"),
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("invalid")))?
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
fn projection_replaces_an_obsolete_owned_cache_and_rebuilds_from_jsonl() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Current durable source",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    let connection = Connection::open(projection.database_path())?;
    connection.pragma_update(None, "user_version", SESSION_CATALOG_SCHEMA_VERSION - 1)?;
    connection.execute(
        "UPDATE session_catalog_entry_v1 SET title = 'stale cache value'",
        [],
    )?;
    drop(connection);

    let report = projection.reconcile()?;
    let entries = projection.list_workspace_entries()?;

    assert!(report.generation_changed);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title.as_deref(), Some("Current durable source"));
    let projection_parent = projection
        .database_path()
        .parent()
        .expect("projection parent");
    assert!(fs::read_dir(projection_parent)?.any(|entry| {
        entry
            .ok()
            .and_then(|entry| entry.file_name().into_string().ok())
            .is_some_and(|name| name.contains("session-catalog-v1.sqlite3-quarantine"))
    }));
    Ok(())
}

#[test]
fn projection_does_not_replace_an_obsolete_cache_while_another_usage_lease_exists() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Current durable source",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    let connection = Connection::open(projection.database_path())?;
    connection.pragma_update(None, "user_version", SESSION_CATALOG_SCHEMA_VERSION - 1)?;
    drop(connection);
    let recovery_lease = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(sqlite_sidecar_path(
            projection.database_path(),
            ".recovery-lock",
        ))?;
    FileExt::try_lock_shared(&recovery_lease)?;

    let error = projection
        .reconcile()
        .expect_err("active usage lease must prevent cache replacement");

    assert!(matches!(error, SessionCatalogProjectionError::RecoveryBusy));
    let connection = Connection::open(projection.database_path())?;
    let user_version: u16 =
        connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    assert_eq!(user_version, SESSION_CATALOG_SCHEMA_VERSION - 1);
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
fn projection_rejects_unknown_persisted_source_diagnostic() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    fs::write(lifecycle.session_dir.join("broken.jsonl"), b"not json\n")?;
    projection.rebuild()?;
    let connection = Connection::open(projection.database_path())?;
    connection.execute(
        "UPDATE session_catalog_entry_v1 SET source_diagnostic = 'future_diagnostic'",
        [],
    )?;
    drop(connection);

    let error = projection
        .list_workspace_entries()
        .expect_err("unknown diagnostic must not be coerced");

    assert!(matches!(
        error,
        SessionCatalogProjectionError::Sqlite { .. }
    ));
    Ok(())
}

#[test]
fn projection_revision_change_rebuilds_without_decoding_retired_row_values() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    finalized_session(
        &lifecycle.session_dir.join("session.jsonl"),
        "Current durable session",
        "deepseek",
        "chat",
    )?;
    projection.rebuild()?;
    assert_eq!(SESSION_CATALOG_PROJECTION_REVISION, 6);
    let connection = Connection::open(projection.database_path())?;
    connection.execute(
        "UPDATE session_catalog_workspace_v1 SET projection_revision = ?1",
        [SESSION_CATALOG_PROJECTION_REVISION - 1],
    )?;
    connection.execute(
        "UPDATE session_catalog_entry_v1 SET source_state = 'unsupported_legacy'",
        [],
    )?;
    drop(connection);

    let report = projection.reconcile()?;
    let rows = projection.list_workspace_entries()?;

    assert!(report.generation_changed);
    assert_eq!(report.removed_source_count, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source_state, LocalSessionCatalogState::Ready);
    assert_eq!(rows[0].title.as_deref(), Some("Current durable session"));
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
fn catalog_mutations_rename_and_delete_exact_durable_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("managed.jsonl");
    let session_id = finalized_session(&source, "Original title", "deepseek", "chat")?;
    projection.rebuild()?;

    let renamed = projection.rename_session("managed.jsonl", &session_id, "Renamed session")?;
    assert!(renamed.projection_generation.is_some());
    assert_eq!(
        projection.list_workspace_entries()?[0].title.as_deref(),
        Some("Renamed session")
    );

    lifecycle.set_session_pin(&source, true, 300)?;
    assert!(matches!(
        projection.delete_session("managed.jsonl", &session_id),
        Err(LocalSessionMutationError::Pinned)
    ));
    lifecycle.set_session_pin(&source, false, 301)?;
    let active_writer = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(source.with_file_name("managed.jsonl.writer-lock"))?;
    FileExt::try_lock_exclusive(&active_writer)?;
    assert!(matches!(
        projection.delete_session("managed.jsonl", &session_id),
        Err(LocalSessionMutationError::WriterBusy)
    ));
    assert!(source.is_file());
    drop(active_writer);
    let deleted = projection.delete_session("managed.jsonl", &session_id)?;
    assert!(deleted.projection_generation.is_some());
    assert!(!source.exists());
    assert!(projection.list_workspace_entries()?.is_empty());
    Ok(())
}

#[test]
fn invalid_catalog_source_can_only_be_quarantined_with_exact_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("broken.jsonl");
    fs::write(&source, b"not a session stream\n")?;
    let resources = lifecycle.session_dir.join("broken");
    fs::create_dir(&resources)?;
    fs::write(resources.join("artifact.bin"), b"durable companion")?;
    projection.rebuild()?;
    let entry = projection.list_workspace_entries()?.remove(0);
    assert_eq!(entry.source_state, LocalSessionCatalogState::Invalid);
    assert_eq!(
        entry.source_diagnostic,
        Some(SessionCatalogSourceDiagnostic::InvalidEventStream)
    );

    assert!(matches!(
        projection.quarantine_invalid_source(
            "broken.jsonl",
            entry.source_bytes + 1,
            entry.source_modified_at_unix_ms,
        ),
        Err(LocalSessionMutationError::IdentityChanged)
    ));
    let receipt = projection.quarantine_invalid_source(
        "broken.jsonl",
        entry.source_bytes,
        entry.source_modified_at_unix_ms,
    )?;

    assert!(!source.exists());
    let quarantine = lifecycle
        .session_dir
        .join(".quarantine")
        .join(receipt.quarantine_name);
    assert!(quarantine.join("session.jsonl").is_file());
    assert_eq!(
        fs::read(quarantine.join("resources/artifact.bin"))?,
        b"durable companion"
    );
    assert!(!resources.exists());
    assert!(
        lifecycle
            .session_dir
            .join("broken.jsonl.writer-lock")
            .is_file()
    );
    assert!(projection.list_workspace_entries()?.is_empty());
    Ok(())
}

#[test]
fn invalid_catalog_source_delete_requires_exact_metadata_and_an_idle_writer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("broken.jsonl");
    fs::write(&source, b"not a session stream\n")?;
    let resources = lifecycle.session_dir.join("broken");
    fs::create_dir(&resources)?;
    fs::write(resources.join("artifact.bin"), b"durable companion")?;
    projection.rebuild()?;
    let entry = projection.list_workspace_entries()?.remove(0);

    assert!(matches!(
        projection.delete_invalid_source(
            "broken.jsonl",
            entry.source_bytes,
            entry.source_modified_at_unix_ms + 1,
        ),
        Err(LocalSessionMutationError::IdentityChanged)
    ));
    let active_store = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(source.with_file_name("broken.jsonl.writer-lock"))?;
    FileExt::try_lock_exclusive(&active_store)?;
    assert!(matches!(
        projection.delete_invalid_source(
            "broken.jsonl",
            entry.source_bytes,
            entry.source_modified_at_unix_ms,
        ),
        Err(LocalSessionMutationError::WriterBusy)
    ));
    assert!(source.exists());
    assert!(resources.exists());
    drop(active_store);
    let receipt = projection.delete_invalid_source(
        "broken.jsonl",
        entry.source_bytes,
        entry.source_modified_at_unix_ms,
    )?;

    assert!(receipt.operation_id.starts_with("invalid-source-delete:"));
    assert!(receipt.projection_generation.is_some());
    assert!(!source.exists());
    assert!(!resources.exists());
    let operation_suffix = receipt
        .operation_id
        .strip_prefix("invalid-source-delete:")
        .expect("operation suffix");
    let tombstone = lifecycle
        .session_dir
        .join(".session-trash")
        .join(format!("invalid-source-delete-{operation_suffix}"));
    assert!(tombstone.join("session.jsonl").is_file());
    assert_eq!(
        fs::read(tombstone.join("resources/artifact.bin"))?,
        b"durable companion"
    );
    assert!(
        lifecycle
            .session_dir
            .join("broken.jsonl.writer-lock")
            .is_file()
    );
    assert!(projection.list_workspace_entries()?.is_empty());
    Ok(())
}

#[cfg(unix)]
#[test]
fn invalid_source_cleanup_rejects_an_unsafe_companion_before_moving_jsonl() -> Result<()> {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("broken.jsonl");
    fs::write(&source, b"not a session stream\n")?;
    let resources = lifecycle.session_dir.join("broken");
    fs::create_dir(&resources)?;
    symlink(temp.path().join("outside"), resources.join("unsafe-link"))?;
    projection.rebuild()?;
    let entry = projection.list_workspace_entries()?.remove(0);

    assert!(matches!(
        projection.delete_invalid_source(
            "broken.jsonl",
            entry.source_bytes,
            entry.source_modified_at_unix_ms,
        ),
        Err(LocalSessionMutationError::Unavailable { .. })
    ));
    assert!(source.is_file());
    assert!(resources.is_dir());
    assert!(!lifecycle.session_dir.join(".session-trash").exists());
    Ok(())
}

#[test]
fn invalid_catalog_source_can_be_permanently_deleted_with_exact_metadata() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let source = lifecycle.session_dir.join("invalid.jsonl");
    fs::write(
        &source,
        format!(
            "{}\n",
            serde_json::to_string(&SessionLogEntry::User(ModelMessage::user("invalid")))?
        ),
    )?;
    projection.rebuild()?;
    let entry = projection.list_workspace_entries()?.remove(0);

    assert_eq!(entry.source_state, LocalSessionCatalogState::Invalid);
    projection.delete_invalid_source(
        "invalid.jsonl",
        entry.source_bytes,
        entry.source_modified_at_unix_ms,
    )?;
    assert!(!source.exists());
    Ok(())
}

#[test]
fn size_limited_catalog_sources_can_be_permanently_deleted() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    fs::write(lifecycle.session_dir.join("oversized.jsonl"), b"0123456789")?;
    fs::write(lifecycle.session_dir.join("scan-limited.jsonl"), b"small")?;
    let projection = SessionCatalogProjectionService::new(
        lifecycle.with_limits(LocalSessionLifecycleLimits {
            max_stream_bytes: 8,
            max_total_validation_bytes: 0,
            ..LocalSessionLifecycleLimits::default()
        }),
        projection.database_path().to_path_buf(),
    );
    projection.rebuild()?;
    let entries = projection.list_workspace_entries()?;

    assert!(
        entries
            .iter()
            .any(|entry| { entry.source_state == LocalSessionCatalogState::Oversized })
    );
    assert!(
        entries
            .iter()
            .any(|entry| { entry.source_state == LocalSessionCatalogState::ScanBudgetExceeded })
    );
    for entry in entries {
        projection.delete_invalid_source(
            &entry.session_ref,
            entry.source_bytes,
            entry.source_modified_at_unix_ms,
        )?;
    }
    assert!(projection.list_workspace_entries()?.is_empty());
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
fn projection_rejects_a_source_that_concatenates_multiple_session_streams() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let (lifecycle, projection) = projection_service(temp.path(), "workspace-1");
    fs::create_dir_all(&lifecycle.session_dir)?;
    let first = temp.path().join("first.jsonl");
    let second = temp.path().join("second.jsonl");
    finalized_session(&first, "First", "deepseek", "chat")?;
    finalized_session(&second, "Second", "anthropic", "claude")?;
    let combined = format!(
        "{}{}",
        fs::read_to_string(first)?,
        fs::read_to_string(second)?
    );
    fs::write(lifecycle.session_dir.join("combined.jsonl"), combined)?;

    projection.rebuild()?;
    let entry = projection.list_workspace_entries()?.remove(0);

    assert_eq!(entry.source_state, LocalSessionCatalogState::Invalid);
    assert_eq!(
        entry.source_diagnostic,
        Some(SessionCatalogSourceDiagnostic::InvalidEventStream)
    );
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
