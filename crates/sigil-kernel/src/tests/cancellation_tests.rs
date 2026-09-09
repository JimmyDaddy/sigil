use std::time::Duration;

use super::{
    RunCancellationFinalizedEntry, RunCancellationOwner, RunCancellationRecorder,
    RunCancellationRequestedEntry, RunCancellationTarget, RunCancellationTerminalOutcome,
    RunEffectClass, RunEffectKind, RunQuiescenceOutcome, append_run_cancellation_finalized,
    append_run_cancellation_requested, reconcile_unfinished_run_cancellations,
};

#[tokio::test]
async fn cancellation_closes_effect_admission_and_waits_for_existing_effects() {
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let effect = handle
        .begin_effect(RunEffectClass::Forward, RunEffectKind::Tool)
        .expect("effect should start before cancellation");
    assert!(owner.request_cancel());
    assert!(!owner.request_cancel());
    assert!(
        handle
            .begin_effect(RunEffectClass::Forward, RunEffectKind::Socket)
            .expect_err("cancelled run must reject a new effect")
            .to_string()
            .contains("refusing new Socket effect")
    );

    assert_eq!(
        owner.wait_for_quiescence(Duration::from_millis(10)).await,
        RunQuiescenceOutcome::TimedOut {
            active_effects: 1,
            active_tasks: 0,
        }
    );
    drop(effect);
    assert_eq!(
        owner.wait_for_quiescence(Duration::from_secs(1)).await,
        RunQuiescenceOutcome::Quiescent
    );
}

#[tokio::test]
async fn cancellation_request_racing_effect_admission_never_leaks_effect_count() {
    for _ in 0..128 {
        let owner = RunCancellationOwner::new();
        let handle = owner.handle();
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let worker_handle = handle.clone();
        let worker_barrier = barrier.clone();
        let worker = tokio::spawn(async move {
            worker_barrier.wait().await;
            worker_handle
                .begin_effect(RunEffectClass::Forward, RunEffectKind::Retry)
                .ok()
        });
        barrier.wait().await;
        owner.request_cancel();
        drop(worker.await.expect("worker should join"));
        assert_eq!(
            owner.wait_for_quiescence(Duration::from_secs(1)).await,
            RunQuiescenceOutcome::Quiescent
        );
        assert_eq!(handle.active_effects(), 0);
    }
}

#[tokio::test]
async fn cancellation_keeps_cleanup_effects_admissible_and_tracks_owned_tasks() {
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    let task = handle.register_task().expect("task should be admitted");
    owner.request_cancel();
    let cleanup = handle
        .begin_effect(RunEffectClass::Cleanup, RunEffectKind::Process)
        .expect("cleanup must remain admissible after cancellation");
    assert_eq!(
        owner.wait_for_quiescence(Duration::from_millis(10)).await,
        RunQuiescenceOutcome::TimedOut {
            active_effects: 1,
            active_tasks: 1,
        }
    );
    drop(cleanup);
    drop(task);
    assert_eq!(
        owner.wait_for_quiescence(Duration::from_secs(1)).await,
        RunQuiescenceOutcome::Quiescent
    );
}

#[tokio::test]
async fn reserved_cancellation_blocks_natural_finalization_before_notification() {
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();
    assert!(owner.reserve_cancel());
    assert!(!handle.try_finalize_naturally());
    assert!(
        handle
            .begin_effect(RunEffectClass::Forward, RunEffectKind::ChildWork)
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), handle.cancelled())
            .await
            .is_err()
    );
    assert!(owner.activate_reserved_cancel());
    handle.cancelled().await;
}

#[test]
fn natural_finalization_is_not_reported_as_a_cancellation_request() {
    let owner = RunCancellationOwner::new();
    let handle = owner.handle();

    assert!(handle.try_finalize_naturally());
    assert!(!handle.is_cancel_requested());
    assert!(!handle.can_request_cancel());
    assert!(!owner.request_cancel());
}

#[test]
fn durable_cancellation_recovery_is_interrupted_and_idempotent() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = crate::Session::new("provider", "model").with_store(store.clone());
    let request = RunCancellationRequestedEntry {
        request_id: "cancel-1".to_owned(),
        run_scope_id: "run-1".to_owned(),
        target: RunCancellationTarget::Run,
        reason: "user request".to_owned(),
        requested_at_ms: 10,
        quiescence_deadline_ms: 20,
    };
    assert!(append_run_cancellation_requested(&mut session, &request)?);
    assert!(!append_run_cancellation_requested(&mut session, &request)?);

    assert!(reconcile_unfinished_run_cancellations(&mut session, 15)?.is_empty());
    let recovered = reconcile_unfinished_run_cancellations(&mut session, 30)?;
    assert_eq!(recovered.len(), 1);
    assert_eq!(
        recovered[0].outcome,
        RunCancellationTerminalOutcome::Interrupted
    );
    assert!(!recovered[0].cleanup_complete);
    assert!(reconcile_unfinished_run_cancellations(&mut session, 40)?.is_empty());

    let mut loaded = crate::Session::load_from_store("provider", "model", store)?;
    assert!(reconcile_unfinished_run_cancellations(&mut loaded, 50)?.is_empty());
    Ok(())
}

#[test]
fn durable_cancellation_terminal_is_exactly_once() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = crate::Session::new("provider", "model").with_store(store);
    let request = RunCancellationRequestedEntry {
        request_id: "cancel-2".to_owned(),
        run_scope_id: "run-2".to_owned(),
        target: RunCancellationTarget::Run,
        reason: "user request".to_owned(),
        requested_at_ms: 10,
        quiescence_deadline_ms: 20,
    };
    assert!(append_run_cancellation_requested(&mut session, &request)?);
    let final_entry = RunCancellationFinalizedEntry {
        request_id: "cancel-2".to_owned(),
        run_scope_id: "run-2".to_owned(),
        outcome: RunCancellationTerminalOutcome::Cancelled,
        cleanup_complete: true,
        active_effects: 0,
        active_tasks: 0,
        reason: "quiescence confirmed".to_owned(),
        finalized_at_ms: 20,
    };
    assert!(append_run_cancellation_finalized(
        &mut session,
        &final_entry
    )?);
    assert!(!append_run_cancellation_finalized(
        &mut session,
        &final_entry
    )?);
    Ok(())
}

#[test]
fn in_memory_cancellation_recorder_is_idempotent_and_scope_bound() -> anyhow::Result<()> {
    let recorder = RunCancellationRecorder::in_memory();
    let request = RunCancellationRequestedEntry {
        request_id: "cancel-memory".to_owned(),
        run_scope_id: "run-memory".to_owned(),
        target: RunCancellationTarget::Run,
        reason: "user request".to_owned(),
        requested_at_ms: 10,
        quiescence_deadline_ms: 20,
    };
    let shared_recorder = recorder.clone();
    assert!(recorder.append_requested(&request)?);
    assert!(!shared_recorder.append_requested(&request)?);

    let cross_scope_request = RunCancellationRequestedEntry {
        run_scope_id: "run-other".to_owned(),
        ..request.clone()
    };
    let error = recorder
        .append_requested(&cross_scope_request)
        .expect_err("request id reuse across scopes must fail");
    assert!(
        error
            .to_string()
            .contains("cancellation request id is reused across run scopes")
    );

    let finalized = RunCancellationFinalizedEntry {
        request_id: request.request_id.clone(),
        run_scope_id: request.run_scope_id.clone(),
        outcome: RunCancellationTerminalOutcome::Cancelled,
        cleanup_complete: true,
        active_effects: 0,
        active_tasks: 0,
        reason: "quiescence confirmed".to_owned(),
        finalized_at_ms: 30,
    };
    assert!(recorder.append_finalized(&finalized)?);
    assert!(!shared_recorder.append_finalized(&finalized)?);

    let missing_request_recorder = RunCancellationRecorder::in_memory();
    let error = missing_request_recorder
        .append_finalized(&finalized)
        .expect_err("finalization without a request must fail");
    assert!(
        error
            .to_string()
            .contains("cancellation terminal requires a matching durable request")
    );

    let cross_scope_recorder = RunCancellationRecorder::in_memory();
    assert!(cross_scope_recorder.append_requested(&request)?);
    let cross_scope_finalized = RunCancellationFinalizedEntry {
        run_scope_id: "run-other".to_owned(),
        ..finalized.clone()
    };
    let error = cross_scope_recorder
        .append_finalized(&cross_scope_finalized)
        .expect_err("terminal scope mismatch must fail");
    assert!(
        error
            .to_string()
            .contains("cancellation terminal scope does not match its request")
    );
    assert!(cross_scope_recorder.append_finalized(&finalized)?);

    Ok(())
}

#[test]
fn concurrent_cancellation_recorders_append_each_phase_once() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let session = crate::Session::new("provider", "model").with_store(store);
    let recorder = session.run_cancellation_recorder()?;
    let request = RunCancellationRequestedEntry {
        request_id: "cancel-concurrent".to_owned(),
        run_scope_id: "run-concurrent".to_owned(),
        target: RunCancellationTarget::Run,
        reason: "race".to_owned(),
        requested_at_ms: 10,
        quiescence_deadline_ms: 20,
    };
    let requested = (0..8)
        .map(|_| {
            let recorder = recorder.clone();
            let request = request.clone();
            std::thread::spawn(move || recorder.append_requested(&request))
        })
        .map(|thread| thread.join().expect("recorder thread should not panic"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    assert_eq!(
        requested.into_iter().filter(|appended| *appended).count(),
        1
    );

    let finalized = RunCancellationFinalizedEntry {
        request_id: request.request_id,
        run_scope_id: request.run_scope_id,
        outcome: RunCancellationTerminalOutcome::Cancelled,
        cleanup_complete: true,
        active_effects: 0,
        active_tasks: 0,
        reason: "quiescent".to_owned(),
        finalized_at_ms: 30,
    };
    let finalized = (0..8)
        .map(|_| {
            let recorder = recorder.clone();
            let finalized = finalized.clone();
            std::thread::spawn(move || recorder.append_finalized(&finalized))
        })
        .map(|thread| thread.join().expect("recorder thread should not panic"))
        .collect::<anyhow::Result<Vec<_>>>()?;
    assert_eq!(
        finalized.into_iter().filter(|appended| *appended).count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn root_issued_stop_reserves_before_audit_without_granting_activation_or_cleanup() {
    let owner = RunCancellationOwner::new();
    let stop = owner.stop_capability();
    let child = owner.handle();
    let effect = child
        .begin_effect(RunEffectClass::Forward, RunEffectKind::Process)
        .expect("admitted effect");
    assert!(stop.reserve());
    assert!(owner.is_cancel_reserved());
    assert!(
        child
            .begin_effect(RunEffectClass::Forward, RunEffectKind::ProviderRequest)
            .is_err()
    );
    assert!(!child.try_finalize_naturally());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), child.cancelled())
            .await
            .is_err(),
        "only the audited root owner activates cancellation"
    );
    assert!(!stop.cleanup_complete());
    assert!(owner.activate_reserved_cancel());
    drop(effect);
    assert_eq!(
        owner.wait_for_quiescence(Duration::from_secs(1)).await,
        RunQuiescenceOutcome::Quiescent
    );
    assert!(stop.cleanup_complete());
    child.mark_cleanup_incomplete();
    assert!(
        !stop.cleanup_complete(),
        "physical cleanup uncertainty survives quiescence"
    );
}
