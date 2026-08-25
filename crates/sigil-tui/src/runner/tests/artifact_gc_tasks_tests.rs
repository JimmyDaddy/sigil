use std::{
    sync::{Arc, mpsc},
    time::Duration,
};

use anyhow::{Context, Result};
use sigil_kernel::{
    ControlEntry, JsonlSessionStore, SessionLogEntry, SessionRef, ToolArtifactGcRootsV1,
    ToolArtifactSensitivity, ToolArtifactStore,
};
use sigil_runtime::LocalSessionLifecycleService;

use super::super::{
    ManagedTuiArtifactStoreLease,
    worker_event::{WorkerEvent, WorkerEventPayloadSender},
    worker_loop::ArtifactGcTaskManager,
};
use super::common::test_authority_composition;

#[test]
fn gc_task_runs_behind_one_typed_completion_event() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    let session_path = sessions.join("session.jsonl");
    let session_store = JsonlSessionStore::new(&session_path)?;
    session_store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "deepseek".to_owned(),
        model_name: "deepseek-v4-flash".to_owned(),
        resolved_model_route: None,
    }))?;
    session_store.append(&SessionLogEntry::Control(ControlEntry::Note {
        kind: "artifact_gc_fixture".to_owned(),
        data: serde_json::Value::Null,
    }))?;
    let (composition, _authority_root) = test_authority_composition(temp.path())?;
    let artifact_lease = ManagedTuiArtifactStoreLease::acquire(
        Arc::clone(&composition.storage_writer),
        &session_path,
        &sigil_kernel::stable_event_uuid("sigil-session-path", &session_path.to_string_lossy()),
    )?;
    let artifact_store: ToolArtifactStore = artifact_lease.store();
    let descriptor = artifact_store.capture_text(
        "call-1",
        "shell",
        "durable output",
        ToolArtifactSensitivity::Ordinary,
    )?;
    let roots = ToolArtifactGcRootsV1 {
        active_result_refs: [descriptor.artifact_ref].into_iter().collect(),
        ..ToolArtifactGcRootsV1::default()
    };
    let session_scope_id = artifact_store.session_scope_id().to_owned();
    drop(artifact_store);
    drop(artifact_lease);
    let lifecycle =
        LocalSessionLifecycleService::new("workspace", &sessions, temp.path().join("exports"))
            .with_lifecycle_journal_path(temp.path().join("lifecycle.jsonl"))
            .with_managed_writer(Arc::clone(&composition.storage_writer), "workspace")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (event_tx, event_rx) = mpsc::channel();
    let mut tasks = ArtifactGcTaskManager::new();
    let attachment = Arc::new(
        sigil_runtime::interactive_session_attachment::InteractiveSessionAttachmentLease::acquire(
            &session_path,
        )?,
    );

    tasks.start(
        &runtime,
        7,
        session_scope_id,
        attachment,
        None,
        WorkerEventPayloadSender::artifact_gc(event_tx),
        lifecycle,
        SessionRef::new_relative("session.jsonl")?,
        roots,
    );

    let event = event_rx
        .recv_timeout(Duration::from_secs(30))
        .context("artifact GC did not publish its completion event within 30 seconds")?;
    let WorkerEvent::ArtifactGcCompleted(result) = event else {
        panic!("artifact GC must publish one typed completion event");
    };
    assert!(tasks.accept_result(result.request_id, &result.session_scope_id));
    let report = result.result.map_err(anyhow::Error::msg)?;
    assert_eq!(report.scanned_manifests, 1);
    assert_eq!(report.retained_manifests, 1);
    assert_eq!(report.tombstoned_manifests, 0);
    assert!(event_rx.try_recv().is_err());
    Ok(())
}
