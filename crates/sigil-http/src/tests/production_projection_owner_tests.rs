use super::*;

#[tokio::test]
async fn production_application_refresh_reads_a_switched_session_without_acquiring_write_authority()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "application-switched-read-owner");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let older = registry.create_session(HttpSessionCreateRequest::default())?;
    let current = registry.create_session(HttpSessionCreateRequest::default())?;
    // A late operation on the previous session displaces the selected session's attachment.
    // The source remains healthy and already works through the formal read-only display path.
    let _older_attachment = driver.acquire_session_attachment(&older)?;
    assert!(driver.application_projection_owner(&current)?.is_none());
    let before = std::fs::read(&current.session_log_path)?;
    let display = registry.conversation_display_page(&current.id, None, 20)?;
    assert!(display.items.is_empty());
    let external =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &current.session_log_path,
        )?;
    let client = registry.application_client(&current.id, "switched-read-client")?;
    let projection = tokio::task::spawn_blocking(move || client.refresh()).await??;
    assert_eq!(projection.conversation.message_count, 0);
    assert!(
        driver.application_projection_owner(&current)?.is_none(),
        "preflight cannot install a writer attachment"
    );
    assert_eq!(std::fs::read(&current.session_log_path)?, before);
    assert!(registry.get_session(&current.id)?.run_ids.is_empty());
    let error = driver
        .admit_run_start(
            &current,
            &HttpRunStartRequest {
                review_annotations: Vec::new(),
                image_attachments: Vec::new(),
                prompt: "must retain exact admission".into(),
                permission_mode: Some(HttpPermissionMode::Manual),
                model_ref: None,
                model_selection_binding: None,
                route_recovery_binding: None,
                reasoning_effort: None,
                reasoning_effort_binding: None,
                skill_binding: None,
                agent_binding: None,
                task_continuation: None,
            },
        )
        .expect_err("read-only preflight cannot authorize a busy writer");
    assert!(matches!(
        error,
        crate::HttpRunAdmissionError::SessionAlreadyActive { .. }
    ));
    drop(external);
    driver.active_runs.lock().expect("active runs").insert(
        "preparing-owner".into(),
        Arc::new(HttpProductionActiveRun {
            session_id: current.id.clone(),
            broker: Arc::new(HttpApprovalBroker::default()),
            cancel_sender: mpsc::unbounded_channel().0,
            projection_owner: Arc::new(Mutex::new(None)),
        }),
    );
    assert!(
        registry
            .application_client(&current.id, "still-preparing")
            .is_err(),
        "an active unpublished owner must not fall back to a detached reader"
    );
    driver.active_runs.lock().expect("active runs").clear();
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn production_display_does_not_wait_for_auxiliary_configuration_or_workspace_reads()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "display-no-auxiliary-io");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let config_path = temp.path().join("sigil.toml");
    let saved_config = std::fs::read(&config_path)?;
    std::fs::remove_file(&config_path)?;
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&config_path)
            .status()?
            .success()
    );
    // Keeping a FIFO writer open makes any accidental RootConfig::load wait for EOF. The
    // controlled blocker is released before asserting, so a regression cannot strand a thread.
    let writer = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&config_path)?;
    let query = tokio::task::spawn_blocking(move || {
        registry.conversation_display_page(&session.id, None, 20)
    });
    let mut query = query;
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), &mut query).await;
    drop(writer);
    std::fs::remove_file(&config_path)?;
    std::fs::write(&config_path, saved_config)?;
    match completed {
        Ok(result) => {
            result??;
        }
        Err(error) => {
            let _ = query.await;
            return Err(error).context("display waited for auxiliary configuration I/O");
        }
    }
    Ok(())
}

#[tokio::test]
async fn production_display_uses_the_shared_index_and_cancelled_reads_leave_it_untouched()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "projection-display-cancel");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let owner = driver
        .application_projection_owner(&session)?
        .expect("actual owner");
    let before = std::fs::read(&session.session_log_path)?;
    let page = registry.conversation_display_page(&session.id, None, 20)?;
    let initialized = owner.metrics()?;
    assert_eq!(initialized.full_prefix_scan_count, 1);
    assert_eq!(
        registry.conversation_display_page(&session.id, None, 20)?,
        page
    );
    assert_eq!(owner.metrics()?.bytes_read, initialized.bytes_read);
    let cancelled = sigil_kernel::SessionReadBudget::default();
    cancelled.cancel();
    assert!(
        registry
            .conversation_display_page_with_budget(&session.id, None, 20, &cancelled)
            .is_err()
    );
    assert!(
        registry
            .transcript_page_with_budget(&session.id, None, 20, &cancelled)
            .is_err()
    );
    assert_eq!(owner.metrics()?.bytes_read, initialized.bytes_read);
    assert_eq!(std::fs::read(&session.session_log_path)?, before);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&session.session_log_path)?
        .write_all(b"{\"partial\":")?;
    for _ in 0..2 {
        assert!(matches!(
            registry.conversation_display_page(&session.id, None, 20),
            Err(crate::HttpRegistryError::ConversationDisplayCorrupt)
        ));
    }
    assert_eq!(
        owner.metrics()?.full_prefix_scan_count,
        1,
        "strict failure does not restart the prefix scan"
    );
    Ok(())
}

#[tokio::test]
async fn production_projection_owner_releases_with_attachment_and_rejects_path_rebinding() {
    let temp = tempfile::tempdir().expect("fixture");
    let driver = production_queue_driver(&temp, "projection-owner-lifecycle");
    let registry = driver
        .build_registry(Arc::new(
            HttpDurableCommandStore::open(temp.path().join("commands.json"), 16).expect("commands"),
        ))
        .expect("registry");
    let first = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("first owned session");
    assert!(
        driver
            .application_projection_owner(&first)
            .expect("owner")
            .is_some()
    );
    let first_bytes = std::fs::read(&first.session_log_path).expect("first session bytes");
    let missing = temp.path().join("unbound-session.jsonl");
    let mut substituted = first.clone();
    substituted.session_log_path = missing.display().to_string();
    assert!(driver.application_projection_owner(&substituted).is_err());
    assert!(
        !missing.exists(),
        "projection lookup cannot construct a writer from a path"
    );

    let second = registry
        .create_session(HttpSessionCreateRequest::default())
        .expect("second owned session");
    assert!(
        driver
            .application_projection_owner(&first)
            .expect("detached session")
            .is_none()
    );
    driver.active_runs.lock().expect("active runs").insert(
        "preparing-revision".to_owned(),
        Arc::new(HttpProductionActiveRun {
            session_id: first.id.clone(),
            broker: Arc::new(HttpApprovalBroker::default()),
            cancel_sender: mpsc::unbounded_channel().0,
            projection_owner: Arc::new(Mutex::new(None)),
        }),
    );
    assert!(
        driver.application_projection_owner(&first).is_err(),
        "an active writer without a published read handle cannot become a detached observer"
    );
    driver.active_runs.lock().expect("active runs").clear();
    assert!(
        driver
            .application_projection_owner(&second)
            .expect("new owner")
            .is_some()
    );
    driver.purge_session_local_state(&second.durable_session_scope_id);
    assert!(
        driver
            .application_projection_owner(&second)
            .expect("purged session")
            .is_none()
    );
    assert_eq!(
        std::fs::read(&first.session_log_path).expect("first session"),
        first_bytes
    );
}

#[tokio::test]
async fn production_application_projection_uses_its_actual_session_owner() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "projection-owner-read");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let session = registry.create_session(HttpSessionCreateRequest::default())?;
    let client = registry.application_client(&session.id, "projection-owner-test")?;
    let store = JsonlSessionStore::new(&session.session_log_path)?;
    let before = std::fs::read(store.path())?;
    let attempts = store.active_projection_metrics().writer_lock_attempt_total;
    tokio::task::spawn_blocking(move || client.refresh()).await??;
    assert!(
        store.active_projection_metrics().writer_lock_attempt_total > attempts,
        "HTTP projection must use the actual session's read coordinator"
    );
    assert_eq!(std::fs::read(store.path())?, before);
    Ok(())
}

#[tokio::test]
async fn production_application_operation_reattaches_an_idle_switched_session_through_its_host()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let driver = production_queue_driver(&temp, "application-switched-write-owner");
    let registry = driver.build_registry(Arc::new(HttpDurableCommandStore::open(
        temp.path().join("commands.json"),
        16,
    )?))?;
    let older = registry.create_session(HttpSessionCreateRequest::default())?;
    let current = registry.create_session(HttpSessionCreateRequest::default())?;
    let current_before = std::fs::read(&current.session_log_path)?;
    let external =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &older.session_log_path,
        )?;
    assert!(driver.application_operation_owner(&older).is_err());
    assert!(
        driver
            .session_attachments
            .lock()
            .expect("attachments")
            .contains_key(&current.durable_session_scope_id),
        "failed reattachment must not discard the selected controller"
    );
    drop(external);
    let binding = sigil_kernel::ApplicationOperationBindingV1::new(
        older.durable_session_scope_id.clone(),
        "1".repeat(64),
        "2".repeat(64),
        sigil_kernel::ApplicationOperationTargetV1::QueuePause { paused: true },
    )?;
    let owner = driver.prepare_application_operation(&older, &binding)?;
    let restored = owner.owner.attach_for_observation()?;
    assert_eq!(restored.session_scope_id(), older.durable_session_scope_id);
    assert!(driver.application_projection_owner(&older)?.is_some());
    assert!(owner.owner.read_handle().read_event_records()?.iter().any(|record| {
        matches!(record.session_log_entry().ok().flatten(),
            Some(sigil_kernel::SessionLogEntry::Control(sigil_kernel::ControlEntry::ApplicationOperationPreparedV1(found))) if found == binding)
    }));
    assert!(
        driver
            .query_application_operation(&older, &binding)?
            .proof
            .is_none(),
        "reattachment and preparation alone cannot settle a domain effect"
    );
    assert_eq!(std::fs::read(&current.session_log_path)?, current_before);
    let released_current =
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &current.session_log_path,
        )?;
    drop(released_current);
    drop(restored);
    drop(owner);
    driver
        .session_attachments
        .lock()
        .expect("attachments")
        .clear();
    let before_wrong_scope = std::fs::read(&older.session_log_path)?;
    let mut wrong_scope = older.clone();
    wrong_scope.durable_session_scope_id = current.durable_session_scope_id;
    assert!(driver.application_operation_owner(&wrong_scope).is_err());
    assert_eq!(
        std::fs::read(&older.session_log_path)?,
        before_wrong_scope,
        "a stale identity must fail before any resume or operation append"
    );
    assert!(
        driver
            .session_attachments
            .lock()
            .expect("attachments")
            .is_empty(),
        "failed host binding must not publish a replacement owner"
    );
    assert!(
        registry.get_session(&older.id)?.run_ids.is_empty(),
        "reattachment must not start a model run"
    );
    Ok(())
}
