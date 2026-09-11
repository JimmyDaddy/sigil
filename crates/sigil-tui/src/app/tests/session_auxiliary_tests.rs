use super::*;
use anyhow::Context;
use sigil_kernel::{JsonlSessionStore, ModelMessage, SessionLogEntry};

#[cfg(unix)]
#[test]
fn catalog_history_preserves_direct_source_alias_for_current_and_resume_selection() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let real_root = fixture.path().join("real");
    let alias_root = fixture.path().join("alias");
    std::fs::create_dir_all(real_root.join("sessions"))?;
    std::os::unix::fs::symlink(&real_root, &alias_root)?;
    let mut config = crate::app::tests::common::test_config();
    config.workspace.root = fixture.path().display().to_string();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let mut paths = app.sigil_paths.clone();
    // The session directory is real; only an ancestor has another spelling, as /var does on macOS.
    paths.session_log_dir = alias_root.join("sessions");
    let current = paths.session_log_dir.join("session-current.jsonl");
    let previous = paths.session_log_dir.join("session-previous.jsonl");
    for (path, prompt) in [(&previous, "previous prompt"), (&current, "current prompt")] {
        let store = JsonlSessionStore::new(path)?;
        let mut session = sigil_kernel::Session::new("fixture", "model").with_store(store);
        session.ensure_identity_entry()?;
        session.append_user_message(ModelMessage::user(prompt))?;
    }
    app.session_log_path = current.clone();
    app.session_browser.history =
        query_session_history(paths, None, &SessionReadBudget::default())?;
    assert!(
        app.session_browser
            .history
            .iter()
            .any(|entry| entry.path == current)
    );
    assert_eq!(app.resolve_resume_target("latest"), Some(previous.clone()));
    assert_eq!(
        app.resolve_resume_target(previous.to_str().context("UTF-8 fixture path")?),
        Some(previous)
    );
    Ok(())
}

#[test]
fn catalog_history_uses_owner_paths_for_direct_and_managed_sessions() -> Result<()> {
    use sigil_runtime::managed_storage_writer::StorageWriterChannelV1;

    fn write_source(path: &std::path::Path, prompt: &str) -> Result<()> {
        let store = JsonlSessionStore::new(path)?;
        let mut session = sigil_kernel::Session::new("fixture", "model").with_store(store);
        session.ensure_identity_entry()?;
        session.append_user_message(ModelMessage::user(prompt))?;
        Ok(())
    }

    let fixture = tempfile::tempdir()?;
    let workspace = fixture.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let config_path = fixture.path().join("sigil.toml");
    std::fs::write(
        &config_path,
        format!(
            "config_version = 2\n[workspace]\nroot = \"{}\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"fixture-model\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
            workspace.display(),
            fixture.path().join("state").display(),
            fixture.path().join("cache").display(),
        ),
    )?;
    let boot = sigil_runtime::application_host::boot_current_schema(&config_path, &workspace)?;
    let paths = boot.resolved_paths().clone();
    let writer = Arc::clone(&boot.composition().storage_writer);
    let direct_path = paths.session_log_dir.join("session-direct.jsonl");
    write_source(&direct_path, "direct history source")?;

    let managed_fixture = fixture.path().join("session-managed-fixture.jsonl");
    write_source(&managed_fixture, "managed history source")?;
    let lease = writer.acquire_named(StorageWriterChannelV1::SessionLog, "session-managed")?;
    let managed_path = lease.path().join("records.jsonl");
    for record in std::fs::read_to_string(&managed_fixture)?.lines() {
        writer.write_record(&lease, record.as_bytes())?;
    }
    writer.finalize(lease)?;

    let history = query_session_history(
        paths.clone(),
        Some(Arc::clone(&writer)),
        &SessionReadBudget::default(),
    )?;
    assert_eq!(history.len(), 2);
    let direct = history
        .iter()
        .find(|entry| entry.title.as_deref() == Some("direct history source"))
        .expect("direct history row");
    let managed = history
        .iter()
        .find(|entry| entry.title.as_deref() == Some("managed history source"))
        .expect("managed history row");
    assert_eq!(direct.path, direct_path);
    assert_eq!(managed.path, managed_path.canonicalize()?);

    let lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        paths.workspace_id.clone(),
        paths.session_log_dir,
        paths.session_exports_root,
    )
    .with_managed_writer(Arc::clone(&writer), paths.workspace_id)?
    .with_managed_session_log_root(writer.managed_leaf_path(StorageWriterChannelV1::SessionLog)?)?;
    for entry in &history {
        let session_id = JsonlSessionStore::read_event_records(&entry.path)?[0]
            .session_id()
            .to_owned();
        let binding = lifecycle.resolve_session_for_reopen(
            &sigil_kernel::SessionRef::new_relative(&entry.label)?,
            &session_id,
        )?;
        assert_eq!(binding.session_log_path, entry.path.canonicalize()?);
    }

    // A configured direct root may legally overlap a managed key directory. Its same-ref direct
    // file must never shadow that key's records.jsonl when selecting a navigation representation.
    let managed_root = managed_path.parent().context("managed source parent")?;
    let managed_key = managed_root
        .file_name()
        .and_then(|name| name.to_str())
        .context("managed source key")?;
    let managed_ref = format!("{managed_key}.jsonl");
    let shadow_path = managed_root.join(&managed_ref);
    write_source(&shadow_path, "direct shadow must not win")?;
    let overlap_lifecycle = sigil_runtime::LocalSessionLifecycleService::new(
        boot.resolved_paths().workspace_id.clone(),
        managed_root,
        boot.resolved_paths().session_exports_root.clone(),
    )
    .with_managed_session_log_root(writer.managed_leaf_path(StorageWriterChannelV1::SessionLog)?)?;
    let overlap_catalog = sigil_runtime::SessionCatalogProjectionService::new(
        overlap_lifecycle,
        fixture.path().join("overlap-catalog.sqlite"),
    );
    let reference = sigil_kernel::SessionRef::new_relative(managed_ref)?;
    let selected_path = overlap_catalog.session_source_path(&reference)?;
    assert_eq!(selected_path, managed_path.canonicalize()?);
    assert_ne!(selected_path, shadow_path);
    let session_id = JsonlSessionStore::read_event_records(&managed_path)?[0]
        .session_id()
        .to_owned();
    assert_eq!(
        overlap_catalog
            .resolve_session_for_reopen(&reference, &session_id)?
            .session_log_path,
        selected_path
    );
    Ok(())
}

#[test]
fn review_reader_reduces_only_the_appended_tail_and_does_not_retain_records() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("first prompt")))?;
    let cursor = Mutex::new(ReviewCursor::default());
    let reader = store.read_handle();
    let first = read_review(&reader, &cursor, &SessionReadBudget::default())?;
    assert!(first.lines.join("\n").contains("first prompt"));
    let first_offset = cursor.lock().expect("cursor").offset;
    let again = read_review(&reader, &cursor, &SessionReadBudget::default())?;
    assert_eq!(again.lines, first.lines);
    assert_eq!(cursor.lock().expect("cursor").offset, first_offset);
    store.append(&SessionLogEntry::User(ModelMessage::user("second prompt")))?;
    let second = read_review(&reader, &cursor, &SessionReadBudget::default())?;
    assert!(second.lines.join("\n").contains("second prompt"));
    assert!(second.lines.join("\n").contains("turn 2/2"));
    assert!(cursor.lock().expect("cursor").offset > first_offset);
    Ok(())
}

#[test]
fn blocked_owned_session_query_keeps_input_responsive_and_cancels_before_lock_release() -> Result<()>
{
    let fixture = tempfile::tempdir()?;
    let config = crate::app::tests::common::test_config();
    let mut app = AppState::from_root_config(&fixture.path().join("sigil.toml"), &config);
    let store = JsonlSessionStore::new(fixture.path().join("session.jsonl"))?;
    store.append(&SessionLogEntry::User(ModelMessage::user("saved prompt")))?;
    app.session_log_path = store.path().to_owned();
    app.attach_session_query_reader(Some(store.read_handle()));
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(store.path())?;
    fs2::FileExt::try_lock_exclusive(&held)?;
    let started = Instant::now();
    app.refresh_session_history();
    app.poll_session_auxiliary();
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(app.session_auxiliary.task.is_some());
    app.handle_worker_message(crate::runner::WorkerMessage::UserInputRequested {
        request: crate::app::tests::worker_bridge_tests::pending_text_user_input_request()?,
        entries: vec![SessionLogEntry::User(ModelMessage::user("saved prompt"))],
    })?;
    assert!(app.pending_user_input().is_some_and(|form| form.open));
    app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ))?;
    assert!(
        matches!(app.pending_user_input().expect("open question").drafts.as_slice(), [crate::app::UserInputDraftValue::Text(value)] if value == "x")
    );
    app.join_session_auxiliary_until(Instant::now() + Duration::from_millis(200))?;
    assert!(started.elapsed() < Duration::from_millis(400));
    fs2::FileExt::unlock(&held)?;
    app.start_bootstrap_session_cleanup()?
        .join()
        .expect("bootstrap cleanup joined");
    assert!(store.path().exists(), "conversation survives cleanup");
    Ok(())
}

#[test]
fn stale_auxiliary_epoch_and_revision_cannot_replace_the_current_session() -> Result<()> {
    let mut app = AppState::from_root_config(
        std::path::Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    app.session_auxiliary
        .reset_scope(&app.session_log_path, &app.session_id);
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(AuxiliaryResult {
        epoch: app.session_auxiliary.epoch.saturating_sub(1),
        revision: app.session_browser.current_entries_revision,
        workspace_snapshot: Some("stale-workspace".to_owned()),
        recovery: None,
        review: None,
        history: Some(Vec::new()),
        child: None,
        child_live_revision: 0,
        notices: vec!["stale failure".to_owned()],
    }))?;
    app.session_auxiliary.task = Some(AuxiliaryTask {
        budget: SessionReadBudget::default(),
        receiver: rx,
        handle: None,
    });
    app.last_notice = Some("current".to_owned());
    app.poll_session_auxiliary();
    assert_ne!(
        app.session_auxiliary.workspace_snapshot.as_deref(),
        Some("stale-workspace")
    );
    assert_eq!(app.last_notice.as_deref(), Some("current"));
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(2))?;
    Ok(())
}

#[test]
fn auxiliary_completion_preserves_question_drafts_and_newly_arrived_queue_members() -> Result<()> {
    let mut app = AppState::from_root_config(
        std::path::Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    let public = crate::app::tests::worker_bridge_tests::pending_text_user_input_request()?;
    let request = sigil_kernel::UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: public.identity,
        source: sigil_kernel::UserInputSourceV1::PlanRevision {
            base_plan_id: sigil_kernel::PlanId::new("draft-plan")?,
            base_plan_hash: format!("sha256:{}", "c".repeat(64)),
        },
        purpose: sigil_kernel::UserInputPurposeV1::RevisionGuidance,
        prompt: public.prompt,
        questions: public.questions,
        allowed_actions: public.allowed_actions,
        requested_at_unix_ms: public.requested_at_unix_ms,
        continuation: None,
    };
    let first = sigil_kernel::UserInputRequestedV1::new(request.clone())?;
    let mut second_request = request;
    second_request.identity.request_id = sigil_kernel::UserInputRequestId::new("second-question")?;
    second_request.identity.source_thread_id = sigil_kernel::AgentThreadId::new("second-thread")?;
    second_request.requested_at_unix_ms += 1;
    let second = sigil_kernel::UserInputRequestedV1::new(second_request)?;
    app.session_browser.current_entries = vec![SessionLogEntry::Control(
        sigil_kernel::ControlEntry::UserInputRequested(Box::new(first)),
    )];
    app.restore_durable_attention_surfaces();
    assert_eq!(app.composer.pending_user_input_queue.len(), 1);
    app.handle_key_event(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('x'),
        crossterm::event::KeyModifiers::NONE,
    ))?;
    app.append_current_session_control(sigil_kernel::ControlEntry::UserInputRequested(Box::new(
        second,
    )));
    app.session_auxiliary
        .reset_scope(&app.session_log_path, &app.session_id);
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(AuxiliaryResult {
        epoch: app.session_auxiliary.epoch,
        revision: app.session_browser.current_entries_revision,
        workspace_snapshot: None,
        recovery: None,
        review: None,
        history: None,
        child: None,
        child_live_revision: 0,
        notices: Vec::new(),
    }))?;
    app.session_auxiliary.task = Some(AuxiliaryTask {
        budget: SessionReadBudget::default(),
        receiver: rx,
        handle: None,
    });
    app.poll_session_auxiliary();
    assert_eq!(app.composer.pending_user_input_queue.len(), 2);
    let active = app
        .pending_user_input()
        .expect("existing question remains selected");
    assert_eq!(active.queue_length, 2);
    assert!(
        matches!(active.drafts.as_slice(), [crate::app::UserInputDraftValue::Text(value)] if value == "x")
    );
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(2))?;
    Ok(())
}

#[test]
fn same_revision_auxiliary_recovery_cannot_reopen_an_applied_answer() -> Result<()> {
    let mut app = AppState::from_root_config(
        std::path::Path::new("sigil.toml"),
        &crate::app::tests::common::test_config(),
    );
    let mut session = sigil_kernel::Session::new("fixture", "model");
    let mut public = crate::app::tests::worker_bridge_tests::pending_text_user_input_request()?;
    public.identity.session_scope_id =
        sigil_kernel::SessionScopeId::new(session.session_scope_id())?;
    let requested = sigil_kernel::UserInputRequestedV1::new(sigil_kernel::UserInputRequestV1 {
        schema_version: sigil_kernel::USER_INPUT_SCHEMA_VERSION,
        identity: public.identity,
        source: public.source,
        purpose: public.purpose,
        prompt: public.prompt,
        questions: public.questions,
        allowed_actions: public.allowed_actions,
        requested_at_unix_ms: 1,
        continuation: Some(sigil_kernel::UserInputContinuationBindingV1 {
            assistant_message_id: "attention-assistant".to_owned(),
            tool_call_id: "attention-call".to_owned(),
            provider_name: "fixture".to_owned(),
            model_name: "model".to_owned(),
        }),
    })?;
    session.append_user_input_lifecycle(vec![
        sigil_kernel::UserInputLifecycleEntryV1::Requested(Box::new(requested.clone())),
    ])?;
    let command = sigil_kernel::UserInputDecisionCommandV1 {
        identity: requested.request.identity.clone(),
        request_hash: requested.request_hash.clone(),
        command_id: sigil_kernel::UserInputCommandId::new("auxiliary-answer")?,
        decision: sigil_kernel::UserInputDecisionV1::Submitted {
            answers: vec![sigil_kernel::UserInputAnswerV1 {
                question_id: "scope".to_owned(),
                value: sigil_kernel::UserInputAnswerValueV1::Text {
                    value: "private answer".to_owned(),
                },
            }],
        },
    };
    sigil_kernel::accept_user_input_decision(&mut session, command.clone(), 2)?;
    let request = sigil_kernel::UserInputProjectionV1::from_session_entries(session.entries())?
        .request(&command.identity)
        .expect("accepted request")
        .public_view();
    app.session_browser.current_entries = session.entries().to_vec();
    app.restore_durable_attention_surfaces();
    app.handle_worker_message(crate::runner::WorkerMessage::UserInputDecisionApplied {
        request,
        continuation_started: true,
        entries: session.entries().to_vec(),
    })?;
    let (tx, rx) = mpsc::channel();
    tx.send(Ok(AuxiliaryResult {
        epoch: app.session_auxiliary.epoch,
        revision: app.session_browser.current_entries_revision,
        workspace_snapshot: None,
        recovery: Some(command.clone()),
        review: None,
        history: None,
        notices: Vec::new(),
        child: None,
        child_live_revision: app.session_auxiliary.child_live_revision,
    }))?;
    app.session_auxiliary.task = Some(AuxiliaryTask {
        budget: SessionReadBudget::default(),
        receiver: rx,
        handle: None,
    });
    app.poll_session_auxiliary();
    assert!(app.pending_user_input().is_none());
    assert!(app.composer.pending_user_input_queue.is_empty());
    assert!(app.user_input_attention_is_submitted(&command.identity, &command.request_hash));
    app.session_auxiliary
        .reset_scope(&app.session_log_path, "different-session");
    assert!(app.session_auxiliary.submitted_user_inputs.is_empty());
    app.join_session_auxiliary_until(Instant::now() + Duration::from_secs(2))?;
    Ok(())
}
