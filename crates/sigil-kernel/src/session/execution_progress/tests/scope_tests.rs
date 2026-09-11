use super::*;

fn record_step_run(
    progress: &mut ExecutionProgress,
    task: &str,
    step: &str,
    ordinal: u32,
) -> Result<String> {
    let task_id = TaskId::new(task)?;
    let step_id = crate::TaskStepId::new(step)?;
    let attempt_id = crate::task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step_id),
        ordinal,
    )?;
    let run_id = crate::task_participant_logical_run_id(&attempt_id);
    progress.apply_control(&ControlEntry::TaskParticipantAttempt(
        crate::TaskParticipantAttemptEntry {
            child_session_ref: crate::task_participant_session_ref(&task_id, &attempt_id)?,
            attempt_id,
            task_id,
            purpose: TaskParticipantPurpose::Step,
            ordinal,
            plan_version: Some(1),
            step_id: Some(step_id),
            role: crate::AgentRole::Executor,
            status: crate::TaskParticipantAttemptStatus::Started,
            reason: None,
        },
    ))?;
    Ok(run_id)
}

fn continuation(root: &str, run: &str) -> Result<crate::UserInputContinuationStartedV1> {
    Ok(crate::UserInputContinuationStartedV1 {
        schema_version: crate::USER_INPUT_SCHEMA_VERSION,
        identity: crate::UserInputIdentityV1 {
            session_scope_id: crate::SessionScopeId::new("evidence-session")?,
            root_logical_run_id: crate::LogicalRunId::new(root)?,
            source_thread_id: crate::AgentThreadId::new("main")?,
            request_id: crate::UserInputRequestId::new("question-one")?,
            generation: 1,
            source_binding_hash: crate::stable_event_hash(b"source"),
        },
        request_hash: crate::stable_event_hash(b"question"),
        claim_id: crate::UserInputClaimId::new("claim-one")?,
        continuation_logical_run_id: crate::LogicalRunId::new(run)?,
        physical_attempt_id: "answer-attempt".to_owned(),
        started_at_unix_ms: 2,
    })
}

#[test]
fn recorded_evidence_groups_task_steps_and_retries_without_foreign_tasks() -> Result<()> {
    let mut progress = ExecutionProgress::default();
    let first = record_step_run(&mut progress, "task", "first", 1)?;
    let retry = record_step_run(&mut progress, "task", "first", 2)?;
    let second = record_step_run(&mut progress, "task", "second", 1)?;
    let foreign = record_step_run(&mut progress, "other-task", "first", 1)?;
    let task_runs = BTreeSet::from([first.clone(), retry.clone(), second.clone()]);
    for run in [&first, &retry, &second] {
        assert_eq!(progress.recorded_evidence_run_ids(run), task_runs);
    }
    assert_eq!(
        progress.recorded_evidence_run_ids(&foreign),
        BTreeSet::from([foreign])
    );
    assert_eq!(
        progress.recorded_evidence_run_ids("ordinary-chat"),
        BTreeSet::from(["ordinary-chat".to_owned()])
    );
    Ok(())
}

#[test]
fn recorded_evidence_groups_direct_attempts_by_bound_task() -> Result<()> {
    let mut progress = ExecutionProgress::default();
    let admission = crate::TaskDirectExecutionAdmittedV1::planner_fallback(
        TaskId::new("direct-task")?,
        "finish the accepted work",
        "planner-attempt",
        1,
    );
    let first = crate::TaskDirectExecutionAttemptV1::started(&admission, 1);
    let retry = crate::TaskDirectExecutionAttemptV1::started(&admission, 2);
    progress.apply_control(&ControlEntry::TaskDirectExecutionAttemptV1(first.clone()))?;
    progress.apply_control(&ControlEntry::TaskDirectExecutionAttemptV1(retry.clone()))?;
    let foreign = record_step_run(&mut progress, "other-task", "first", 1)?;
    let first_run = crate::task_direct_execution_logical_run_id(&first.attempt_id);
    let retry_run = crate::task_direct_execution_logical_run_id(&retry.attempt_id);
    assert_eq!(
        progress.recorded_evidence_run_ids(&retry_run),
        BTreeSet::from([first_run, retry_run])
    );
    assert_eq!(
        progress.recorded_evidence_run_ids(&foreign),
        BTreeSet::from([foreign])
    );
    Ok(())
}

#[test]
fn recorded_evidence_follows_durable_input_continuations_without_rebinding_roots() -> Result<()> {
    let mut progress = ExecutionProgress::default();
    let root = record_step_run(&mut progress, "task", "first", 1)?;
    let sibling = record_step_run(&mut progress, "task", "second", 1)?;
    let started = continuation(&root, "answer-run")?;
    progress.apply_control(&ControlEntry::UserInputContinuationStarted(started.clone()))?;
    progress.apply_control(&ControlEntry::UserInputContinuationStarted(started))?;
    let expected = BTreeSet::from([root.clone(), sibling, "answer-run".to_owned()]);
    assert_eq!(progress.recorded_evidence_run_ids(&root), expected);
    assert_eq!(progress.recorded_evidence_run_ids("answer-run"), expected);
    let conflicting = continuation("foreign-root", "answer-run")?;
    assert!(
        progress
            .apply_control(&ControlEntry::UserInputContinuationStarted(conflicting))
            .is_err()
    );
    assert_eq!(progress.recorded_evidence_run_ids("answer-run"), expected);
    Ok(())
}
