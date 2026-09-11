use crate::runner::protocol::WorkerStopControl;
use crate::runner::worker_loop::finalize_completed_run_cancellation;
use crate::runner::{WorkerCommand, WorkerCommandSender};
use sigil_kernel::{RunCancellationOwner, RunEffectClass, RunEffectKind};

#[test]
fn failed_automatic_route_can_shutdown_without_a_stale_stop_target() -> anyhow::Result<()> {
    use super::common::{PlannedProvider, StreamPlan, spawn_test_worker, test_root_config};
    use crate::runner::WorkerMessage;
    use sigil_kernel::{Agent, ReasoningEffort, TaskRoutingPolicy, ToolRegistry};

    let temp = tempfile::tempdir()?;
    let mut config = test_root_config(temp.path(), "planned", "planned-model");
    config.task.routing_policy = TaskRoutingPolicy::Auto;
    let worker = spawn_test_worker(
        config,
        temp.path().join("session.jsonl"),
        Agent::new(
            PlannedProvider::new(vec![StreamPlan::Fail("provider unavailable")]),
            ToolRegistry::new(),
        ),
        temp.path().to_path_buf(),
    )?;
    worker.send(WorkerCommand::SubmitPrompt {
        prompt: "hello".to_owned(),
        reasoning_effort: ReasoningEffort::Medium,
    })?;
    let failure = worker.recv_until(|message| matches!(message, WorkerMessage::RunFailed(_)))?;
    assert!(
        matches!(failure, WorkerMessage::RunFailed(error) if error.contains("provider unavailable"))
    );
    let sender = worker.command_sender();
    worker.shutdown()?;
    assert!(
        sender.cleanup_complete(),
        "{}",
        sender.shutdown_diagnostic("sigil-agent-worker")
    );
    Ok(())
}

#[test]
fn urgent_stop_reserves_the_actual_owner_before_worker_dispatch() -> anyhow::Result<()> {
    let (sender, receiver) = WorkerCommandSender::test_channel();
    let owner = RunCancellationOwner::new();
    sender.stop_control().bind(&owner);
    sender.send(WorkerCommand::CancelRun)?;
    assert!(owner.is_cancel_reserved());
    assert!(
        owner
            .handle()
            .begin_effect(RunEffectClass::Forward, RunEffectKind::Tool)
            .is_err()
    );
    assert!(matches!(receiver.try_recv()?, WorkerCommand::CancelRun));
    assert!(!sender.cleanup_complete());
    owner.activate_reserved_cancel();
    assert!(sender.cleanup_complete());
    Ok(())
}

#[test]
fn closing_reserves_a_concurrently_admitted_root_and_rejects_new_commands() -> anyhow::Result<()> {
    let (sender, receiver) = WorkerCommandSender::test_channel();
    sender.reserve_stop(true);
    let owner = RunCancellationOwner::new();
    let root_task = owner.handle().register_task()?;
    sender.stop_control().bind(&owner);
    assert!(owner.is_cancel_reserved());
    assert!(owner.handle().register_task().is_err());
    owner.activate_reserved_cancel();
    assert!(!sender.cleanup_complete());
    drop(root_task);
    assert!(sender.cleanup_complete());
    assert!(
        sender
            .send(WorkerCommand::SubmitPrompt {
                prompt: "late".to_owned(),
                reasoning_effort: sigil_kernel::ReasoningEffort::Medium
            })
            .is_err()
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let unattached = WorkerStopControl::default();
    let unrelated = RunCancellationOwner::new();
    unattached.reserve(true);
    assert!(
        !unrelated.is_cancel_reserved(),
        "cached session identity never grants stop authority"
    );
    Ok(())
}

#[test]
fn completed_owner_and_idle_worker_shutdown_have_no_false_cleanup_failure() -> anyhow::Result<()> {
    let (idle, _) = WorkerCommandSender::test_channel();
    idle.reserve_stop(true);
    assert!(idle.cleanup_complete());

    let (sender, _) = WorkerCommandSender::test_channel();
    let owner = RunCancellationOwner::new();
    let root = owner.handle().register_task()?;
    sender.stop_control().bind(&owner);
    assert!(owner.handle().try_finalize_naturally());
    drop(root);
    sender.reserve_stop(true);
    assert!(!owner.is_cancel_reserved());
    assert!(sender.cleanup_complete());
    Ok(())
}

#[test]
fn completion_preserves_an_earlier_stop_and_cleanup_failure_evidence() -> anyhow::Result<()> {
    let (sender, _) = WorkerCommandSender::test_channel();
    let stopped = RunCancellationOwner::new();
    sender.stop_control().bind(&stopped);
    sender.reserve_stop(false);
    assert!(!finalize_completed_run_cancellation(&stopped));
    assert!(stopped.is_cancel_reserved());
    assert!(!sender.cleanup_complete());
    assert!(stopped.activate_reserved_cancel());
    assert!(sender.cleanup_complete());

    let incomplete = RunCancellationOwner::new();
    let pending = incomplete.handle().register_task()?;
    incomplete.handle().mark_cleanup_incomplete();
    sender.stop_control().bind(&incomplete);
    assert!(finalize_completed_run_cancellation(&incomplete));
    sender.reserve_stop(true);
    assert!(!incomplete.is_cancel_reserved());
    assert!(!sender.cleanup_complete());
    drop(pending);
    assert!(!sender.cleanup_complete());
    Ok(())
}

#[test]
fn failed_unpublished_admission_does_not_leave_an_idle_stop_target() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let session = sigil_kernel::Session::load_from_store(
        "test",
        "test",
        sigil_kernel::JsonlSessionStore::new(temp.path().join("admission.jsonl"))?,
    )?;
    let (sender, _) = WorkerCommandSender::test_channel();
    let (owner, _recorder, _handle, guard) =
        crate::runner::worker_loop::prepare_run_cancellation(&session)
            .map_err(anyhow::Error::msg)?;
    // Route/preflight failure drops this unpublished preparation; only a successful spawn binds.
    drop(guard);
    sender.reserve_stop(true);
    assert!(!owner.is_cancel_reserved());
    assert!(sender.cleanup_complete());
    Ok(())
}

#[test]
fn shutdown_preserves_incomplete_prior_owners_and_the_first_shared_deadline() -> anyhow::Result<()>
{
    use crate::runner::protocol::WorkerShutdownStage;
    use std::time::{Duration, Instant};
    let (sender, _) = WorkerCommandSender::test_channel();
    let first = RunCancellationOwner::new();
    let first_task = first.handle().register_task()?;
    sender.stop_control().bind(&first);
    first.request_cancel();
    let second = RunCancellationOwner::new();
    let second_task = second.handle().register_task()?;
    sender.stop_control().bind(&second);
    let deadline = Instant::now() + Duration::from_millis(50);
    sender.begin_shutdown_until(deadline);
    sender.begin_shutdown_until(deadline + Duration::from_secs(10));
    assert_eq!(sender.stop_control().shutdown_deadline(), Some(deadline));
    sender
        .stop_control()
        .stage(WorkerShutdownStage::RunQuiescence);
    assert!(second.activate_reserved_cancel());
    drop(second_task);
    let diagnostic = sender.shutdown_diagnostic("sigil-agent-worker");
    assert!(diagnostic.contains("stage=run-quiescence"));
    assert!(diagnostic.contains("elapsed_ms="));
    assert!(diagnostic.contains("active_tasks=1"));
    assert!(!sender.cleanup_complete());
    drop(first_task);
    assert!(sender.cleanup_complete());
    Ok(())
}
