use super::*;

#[tokio::test]
async fn production_artifact_owned_store_requires_one_registered_scope_and_path_match() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let driver = production_queue_driver(&temp, "artifact-owner-binding");
    let mut session = production_authority_session(&driver, &temp, "artifact-bound-owner");
    let other = production_authority_session(&driver, &temp, "artifact-bound-other");
    let run_id = "registered-run".to_owned();
    session.run_ids.push(run_id.clone());
    let lease = authority_artifact_store_for_session(&driver.services, &session)
        .expect("exact-session artifact lease");
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .insert("unregistered-run".to_owned(), lease.store());
    assert!(
        driver
            .owned_tool_artifact_store(&session)
            .expect("ignore unregistered runs")
            .is_none()
    );
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .insert(run_id.clone(), lease.store());
    assert_eq!(
        driver
            .owned_tool_artifact_store(&session)
            .expect("valid owner")
            .expect("one owner")
            .0,
        run_id
    );
    session.run_ids.push("second-registered-run".to_owned());
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .insert("second-registered-run".to_owned(), lease.store());
    assert!(matches!(
        driver.owned_tool_artifact_store(&session),
        Err(crate::HttpToolArtifactReadDriverError::Unavailable)
    ));
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .clear();
    session.run_ids.truncate(1);
    let other_lease = authority_artifact_store_for_session(&driver.services, &other)
        .expect("other-session artifact lease");
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .insert(run_id.clone(), other_lease.store());
    assert!(matches!(
        driver.owned_tool_artifact_store(&session),
        Err(crate::HttpToolArtifactReadDriverError::Unavailable)
    ));
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .clear();
    drop(other_lease);
    drop(lease);

    let key = authority_artifact_store_key(&driver.services, &session).expect("artifact key");
    let wrong_path = sigil_runtime::managed_artifact_store::ManagedArtifactStoreLeaseV1::acquire_with_session_path(
        Arc::clone(&driver.services.authority_composition().expect("authority").storage_writer),
        &key,
        &session.durable_session_scope_id,
        std::path::PathBuf::from(&other.session_log_path),
    ).expect("fixture lease with a conflicting logical path");
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .insert(run_id, wrong_path.store());
    let rejected_path = driver.owned_tool_artifact_store(&session);
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .clear();
    drop(wrong_path);
    assert!(matches!(
        rejected_path,
        Err(crate::HttpToolArtifactReadDriverError::Unavailable)
    ));
}

#[tokio::test]
async fn production_artifact_read_reuses_exact_session_store_after_foreground_is_cleared() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let driver = production_queue_driver(&temp, "artifact-terminal-handoff");
    let mut session = production_authority_session(&driver, &temp, "artifact-terminal-owner");
    let descriptor = append_durable_tool_artifact(
        &driver,
        &session,
        ToolResult::ok(
            "call-owned",
            "read_file",
            "terminal saved output",
            ToolResultMeta::default(),
        ),
    );
    let run_id = "registered-terminal-run".to_owned();
    session.run_ids.push(run_id.clone());
    assert!(session.foreground_run_id.is_none());
    let request = crate::HttpToolArtifactReadRequest {
        artifact_ref: descriptor.artifact_ref.artifact_id,
        selector: crate::HttpToolArtifactSelector::ByteSlice {
            offset: 0,
            limit: 64,
        },
    };
    // A durable terminal has cleared the foreground owner, while its supervisor still owns
    // the real artifact namespace. Re-admission is forbidden until this lease is finalized.
    let lease = authority_artifact_store_for_session(&driver.services, &session)
        .expect("terminal supervisor artifact lease");
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .insert(run_id, lease.store());
    let before_release = driver.tool_artifact_page(&session, &request);
    drop(lease);
    // The map may briefly retain a closed facade. Its known run follows the existing release
    // wait, then reads through a newly admitted exact-session namespace.
    let after_release = driver.tool_artifact_page(&session, &request);
    driver
        .active_artifact_stores
        .lock()
        .expect("active stores")
        .clear();
    assert_eq!(
        before_release
            .expect("read through the still-owned capability")
            .body,
        "terminal saved output"
    );
    assert_eq!(
        after_release
            .expect("read after physical lease release")
            .body,
        "terminal saved output"
    );
}

#[tokio::test]
async fn production_artifact_reads_serialize_same_session_leases_and_keep_other_sessions_live() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let driver = production_queue_driver(&temp, "artifact-read-coordination");
    let session = production_authority_session(&driver, &temp, "artifact-read-owner");
    let other = production_authority_session(&driver, &temp, "artifact-read-other");
    let request_for = |session: &HttpSessionSnapshot| {
        let descriptor = append_durable_tool_artifact(
            &driver,
            session,
            ToolResult::ok(
                "call-read",
                "read_file",
                "saved output\n",
                ToolResultMeta::default(),
            ),
        );
        crate::HttpToolArtifactReadRequest {
            artifact_ref: descriptor.artifact_ref.artifact_id,
            selector: crate::HttpToolArtifactSelector::ByteSlice {
                offset: 0,
                limit: 64,
            },
        }
    };
    let request = request_for(&session);
    let other_request = request_for(&other);
    let key = authority_artifact_store_key(&driver.services, &session).expect("artifact key");
    // Pin exactly the coordinator permit and real authority lease held by a display read.
    // The competing public driver methods must wait before attempting a second admission.
    let permit = driver
        .artifact_access
        .begin_read(&key, &sigil_kernel::SessionReadBudget::default())
        .expect("first display reader permit");
    let lease = authority_artifact_store_for_session(&driver.services, &session)
        .expect("first display reader owns the physical namespace");
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let page = {
        let driver = Arc::clone(&driver);
        let session = session.clone();
        let started = started_tx.clone();
        tokio::task::spawn_blocking(move || {
            started.send(()).expect("start receiver");
            driver.tool_artifact_page(&session, &request)
        })
    };
    let display = {
        let driver = Arc::clone(&driver);
        let session = session.clone();
        tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("start receiver");
            driver.conversation_display_page(&session, None, 20)
        })
    };
    let mut other_page = {
        let driver = Arc::clone(&driver);
        tokio::task::spawn_blocking(move || driver.tool_artifact_page(&other, &other_request))
    };
    for _ in 0..2 {
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("reader started");
    }
    let other_completed = tokio::time::timeout(Duration::from_secs(2), &mut other_page).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let same_session_waited = !page.is_finished() && !display.is_finished();
    // Always release the actual lease before its permit, including on a failing assertion.
    drop(lease);
    drop(permit);
    let page = page.await.expect("page worker");
    let display = display.await.expect("display worker");
    let other_completed = match other_completed {
        Ok(result) => Some(result.expect("other page worker")),
        Err(_) => {
            let _ = other_page.await;
            None
        }
    };
    assert!(
        same_session_waited,
        "concurrent readers raced the same authority namespace"
    );
    assert_eq!(
        page.expect("same-session artifact read").body,
        "saved output\n"
    );
    display.expect("same-session display read");
    assert_eq!(
        other_completed
            .expect("another session must not wait")
            .expect("other artifact read")
            .body,
        "saved output\n"
    );
}

#[tokio::test]
async fn production_display_cancelled_while_waiting_for_an_artifact_reader_does_not_claim_a_lease()
{
    let temp = tempfile::tempdir().expect("temporary directory");
    let driver = production_queue_driver(&temp, "artifact-read-cancel");
    let session = production_authority_session(&driver, &temp, "artifact-cancel-owner");
    let key = authority_artifact_store_key(&driver.services, &session).expect("artifact key");
    let permit = driver
        .artifact_access
        .begin_read(&key, &sigil_kernel::SessionReadBudget::default())
        .expect("first reader permit");
    let lease = authority_artifact_store_for_session(&driver.services, &session)
        .expect("first reader authority lease");
    let writer = &driver
        .services
        .authority_composition()
        .expect("authority")
        .storage_writer;
    let markers = [
        sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStaging,
        sigil_runtime::managed_storage_writer::StorageWriterChannelV1::ArtifactStore,
    ]
    .map(|channel| {
        writer
            .managed_named_leaf_path(channel, &key)
            .expect("namespace")
            .join("authority-admission.json")
    });
    let before = markers
        .each_ref()
        .map(|path| std::fs::read(path).expect("admission marker"));
    let budget = sigil_kernel::SessionReadBudget::default();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let mut query = {
        let driver = Arc::clone(&driver);
        let budget = budget.clone();
        tokio::task::spawn_blocking(move || {
            started_tx.send(()).expect("start receiver");
            driver.conversation_display_page_with_budget(&session, None, 20, &budget)
        })
    };
    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("reader started");
    tokio::time::sleep(Duration::from_millis(100)).await;
    let waited = !query.is_finished();
    budget.cancel();
    let cancelled = tokio::time::timeout(Duration::from_secs(2), &mut query).await;
    let after = markers
        .each_ref()
        .map(|path| std::fs::read(path).expect("admission marker"));
    drop(lease);
    drop(permit);
    let cancelled = match cancelled {
        Ok(result) => Some(result.expect("display worker")),
        Err(_) => {
            let _ = query.await;
            None
        }
    };
    assert!(waited, "display must wait for the occupied namespace");
    assert_eq!(
        cancelled.expect("cancellation must finish while the holder is still active"),
        Err(crate::HttpConversationDisplayDriverError::Unavailable)
    );
    assert_eq!(
        after, before,
        "a cancelled waiter cannot claim either artifact namespace"
    );
}
