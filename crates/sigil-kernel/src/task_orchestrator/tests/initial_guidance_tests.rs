use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use crate::task_orchestrator::task_orchestrator_child_session_test_support::{
    test_step_completion_claim, test_synthesis_completion_claim,
};

const GUIDANCE: &str = "Keep the original objective and inspect the retry boundary first";

fn seeded_session(store: crate::JsonlSessionStore) -> Result<(Session, SequentialTaskRequest)> {
    let mut session = Session::load_from_store("test", "model", store)?;
    let request = SequentialTaskRequest {
        task_id: TaskId::new("initial-guidance-task")?,
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "Inspect the Task execution lifecycle".to_owned(),
    };
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: request.task_id.clone(),
        parent_session_ref: request.parent_session_ref.clone(),
        objective: request.objective.clone(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    Ok((session, request))
}

fn source(session: &Session) -> Result<crate::ConversationTurnRef> {
    crate::ConversationTurnRef::new(
        session.session_scope_id(),
        "guidance-source",
        "guidance-root-run",
    )
}

fn options(root: &Path) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: root.to_path_buf(),
        max_turns: Some(4),
        tool_timeout_secs: 5,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: crate::InteractionMode::Headless,
        permission_config: crate::PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: crate::MemoryConfig::with_enabled(false),
        compaction_config: crate::CompactionConfig::default(),
        permission_mode_override: None,
        tool_authority: None,
    }
}

fn plan(task_id: TaskId) -> Result<TaskPlanEntry> {
    Ok(TaskPlanEntry {
        task_id,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("inspect")?,
            title: "Inspect the requested boundary".to_owned(),
            display_name: None,
            detail: None,
            role: crate::AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: Some(TaskStepMode::Read),
            isolation: Some(crate::TaskIsolationMode::SharedReadOnly),
        }],
        reason: None,
    })
}

#[derive(Default)]
struct GuidanceRunner {
    inputs: Arc<Mutex<Vec<AgentRunInput>>>,
    calls: Arc<AtomicUsize>,
    dispatched_steps: Arc<AtomicUsize>,
    fail_first: bool,
    force_replan: bool,
}

#[async_trait]
impl TaskChildSessionRunner for GuidanceRunner {
    async fn run_planner_session<H, A>(
        &self,
        _session: &mut Session,
        request: TaskPlannerSessionRunRequest,
        _handler: &mut H,
        _approval: &mut A,
    ) -> Result<TaskPlannerSessionRunOutcome>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.inputs
            .lock()
            .expect("input capture")
            .push(request.child_input.clone());
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_first && ordinal == 0 {
            return Err(crate::ProviderTurnRecoveryTerminalError {
                disposition: crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked,
                reason_code: "fixture_planner_recovery_blocked",
            }
            .into());
        }
        let mut accepted_plan = plan(request.task.task_id)?;
        if self.force_replan
            && let Some(assessment) = request.child_input.task_guidance_assessment.as_ref()
        {
            accepted_plan.plan_version = assessment.plan_version + 1;
            accepted_plan.steps[0].title = "Inspect the latest requested boundary".to_owned();
        }
        let guidance_applied = request
            .child_input
            .task_guidance_assessment
            .filter(|_| !self.force_replan)
            .map(|assessment| crate::TaskGuidanceAppliedEntry {
                queue_id: assessment.queue_id,
                task_id: assessment.task_id,
                plan_version: assessment.plan_version,
                dispatch_run_id: assessment.dispatch_run_id,
                reason: crate::TaskGuidanceApplyReason::AddsExecutionConstraint,
                target_step_ids: assessment.eligible_pending_step_ids,
            });
        Ok(TaskPlannerSessionRunOutcome::Accepted(Box::new(
            TaskPlannerSessionRunOutput {
                attempt_id: request.attempt_id,
                accepted_plan,
                step_contracts: Vec::new(),
                guidance_applied,
                child_session_ref: request.child_session_ref,
            },
        )))
    }

    async fn run_child_session<H, A>(
        &self,
        _session: &mut Session,
        request: TaskChildSessionRunRequest,
        _handler: &mut H,
        _approval: &mut A,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        self.dispatched_steps.fetch_add(1, Ordering::SeqCst);
        let completion_claim = test_step_completion_claim(&request);
        Ok(TaskChildSessionRunOutput {
            attempt_id: request.attempt_id,
            final_text: "Inspection complete".to_owned(),
            outcome: crate::AgentRunOutcome::default(),
            child_session_ref: request.child_session_ref,
            final_answer_ref: None,
            completion_claim: Some(completion_claim),
            artifact_refs: Vec::new(),
            changeset_proposal: None,
            isolated_parent_snapshot_id: None,
        })
    }

    async fn run_synthesis_session<H, A>(
        &self,
        _session: &mut Session,
        request: TaskSynthesisSessionRunRequest,
        _handler: &mut H,
        _approval: &mut A,
    ) -> Result<TaskSynthesisSessionRunOutput>
    where
        H: EventHandler + Send,
        A: ApprovalHandler + Send,
    {
        let final_text = "Task inspection complete".to_owned();
        let completion_claim = test_synthesis_completion_claim(&request);
        Ok(TaskSynthesisSessionRunOutput {
            attempt_id: request.attempt_id,
            outcome: crate::AgentRunOutcome::default(),
            child_session_ref: request.child_session_ref.clone(),
            final_answer_ref: crate::AgentFinalAnswerRef {
                session_ref: request.child_session_ref,
                message_id: "initial-guidance-final".to_owned(),
                content_hash: super::super::hash_text(&final_text),
                char_count: final_text.chars().count(),
            },
            artifact_refs: Vec::new(),
            completion_claim: Some(completion_claim),
            final_text,
        })
    }
}

#[tokio::test]
async fn accepted_initial_guidance_survives_reload_and_reaches_planner_once() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let source = source(&session)?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &source,
        &mut crate::NoopEventHandler,
    )?;
    let accepted_count = session.entries().len();
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &source,
        &mut crate::NoopEventHandler,
    )?;
    assert_eq!(session.entries().len(), accepted_count);
    let runner = GuidanceRunner::default();
    let inputs = Arc::clone(&runner.inputs);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let opts = options(root.path());
    let output = Box::pin(orchestrator.run_with_initial_guidance(
        &mut session,
        request.clone(),
        opts.clone(),
        opts.clone(),
        opts.clone(),
        opts,
        8,
        None,
        &mut crate::NoopEventHandler,
        &mut crate::AutoApproveHandler,
    ))
    .await?;
    assert_eq!(output.status, TaskRunStatus::Completed);
    let inputs = inputs.lock().expect("input capture");
    assert_eq!(inputs.len(), 1);
    assert!(
        inputs[0]
            .persisted_user_message
            .as_deref()
            .is_some_and(|prompt| {
                prompt.contains(GUIDANCE) && prompt.contains(&request.objective)
            })
    );
    let session = Session::load_from_store("test", "model", store)?;
    let guidance =
        initial_task_guidance(&session, &request.task_id, None)?.expect("durable guidance");
    assert!(initial_guidance_consumed_by_plan(&session, &guidance));
    assert!(guidance.selection.plan_version.is_none());
    assert_eq!(
        session.task_state_projection().tasks[&request.task_id].objective,
        request.objective
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(entry,
                SessionLogEntry::User(message) if message.id == source.message_id
            ))
            .count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn recovering_planner_replays_before_later_guidance_and_commits_real_settlement() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let original = begin_participant_attempt(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        None,
        crate::AgentRole::Planner,
        None,
    )?;
    let source = source(&session)?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &source,
        &mut crate::NoopEventHandler,
    )?;
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    let runner = GuidanceRunner::default();
    let inputs = Arc::clone(&runner.inputs);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let opts = options(root.path());
    let output = Box::pin(orchestrator.run_with_initial_guidance(
        &mut session,
        request.clone(),
        opts.clone(),
        opts.clone(),
        opts.clone(),
        opts,
        8,
        None,
        &mut crate::NoopEventHandler,
        &mut crate::AutoApproveHandler,
    ))
    .await?;
    assert_eq!(output.status, TaskRunStatus::Completed);
    let inputs = inputs.lock().expect("input capture");
    assert_eq!(inputs.len(), 2);
    assert!(inputs[0].persisted_user_message.is_none());
    assert!(inputs[0].transient_context.is_empty());
    assert_eq!(
        inputs[0].logical_run_id(),
        Some(task_participant_logical_run_id(&original.attempt_id).as_str())
    );
    assert!(inputs[1].task_guidance_assessment.is_some());
    assert!(
        inputs[1]
            .persisted_user_message
            .as_deref()
            .is_some_and(|prompt| prompt.contains(GUIDANCE))
    );
    let session = Session::load_from_store("test", "model", store)?;
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskGuidanceMaterialized(_))
            ))
            .count(),
        1
    );
    assert!(recoverable_task_guidance_review(&session, &request.task_id, None)?.is_none());
    Ok(())
}

#[test]
fn initial_guidance_recovery_covers_plan_commit_before_guidance_review() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let original = begin_participant_attempt(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        None,
        crate::AgentRole::Planner,
        None,
    )?;
    let source = source(&session)?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &source,
        &mut crate::NoopEventHandler,
    )?;
    session.append_control(ControlEntry::TaskPlan(plan(request.task_id.clone())?))?;
    let mut completed = original;
    completed.status = TaskParticipantAttemptStatus::Completed;
    session.append_control(ControlEntry::TaskParticipantAttempt(completed))?;
    let session = Session::load_from_store("test", "model", store)?;
    let review = recoverable_task_guidance_review(&session, &request.task_id, None)?
        .expect("pending later guidance");
    assert_eq!(review.guidance, GUIDANCE);
    assert!(recoverable_task_guidance_review_retry_controls(&session, &review)?.is_empty());
    Ok(())
}

#[test]
fn initial_guidance_rejects_cross_session_changed_source_and_stale_generation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (mut session, request) = seeded_session(crate::JsonlSessionStore::new(
        root.path().join("session.jsonl"),
    )?)?;
    let source = source(&session)?;
    let foreign =
        crate::ConversationTurnRef::new("foreign-session", "guidance-source", "guidance-root-run")?;
    let before = session.entries().len();
    assert!(
        accept_initial_continuation_guidance(
            &mut session,
            &request,
            GUIDANCE,
            &foreign,
            &mut crate::NoopEventHandler
        )
        .is_err()
    );
    assert_eq!(session.entries().len(), before);
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &source,
        &mut crate::NoopEventHandler,
    )?;
    let accepted = session.entries().len();
    assert!(
        accept_initial_continuation_guidance(
            &mut session,
            &request,
            "Changed guidance",
            &source,
            &mut crate::NoopEventHandler
        )
        .is_err()
    );
    let mut changed = request.clone();
    changed.objective = "Replace the task objective".to_owned();
    assert!(
        accept_initial_continuation_guidance(
            &mut session,
            &changed,
            GUIDANCE,
            &source,
            &mut crate::NoopEventHandler
        )
        .is_err()
    );
    assert_eq!(session.entries().len(), accepted);
    session.append_control(ControlEntry::TaskPlan(plan(request.task_id.clone())?))?;
    assert!(
        accept_initial_continuation_guidance(
            &mut session,
            &request,
            GUIDANCE,
            &source,
            &mut crate::NoopEventHandler
        )
        .is_err()
    );
    Ok(())
}

#[tokio::test]
async fn initial_guidance_after_a_paused_planner_keeps_both_sources_and_latest_instruction_order()
-> Result<()> {
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let first_source = source(&session)?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &first_source,
        &mut crate::NoopEventHandler,
    )?;
    let runner = GuidanceRunner {
        fail_first: true,
        ..GuidanceRunner::default()
    };
    let inputs = Arc::clone(&runner.inputs);
    let steps = Arc::clone(&runner.dispatched_steps);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let opts = options(root.path());
    let first = Box::pin(orchestrator.run_with_initial_guidance(
        &mut session,
        request.clone(),
        opts.clone(),
        opts.clone(),
        opts.clone(),
        opts.clone(),
        8,
        None,
        &mut crate::NoopEventHandler,
        &mut crate::AutoApproveHandler,
    ))
    .await?;
    assert_eq!(first.status, TaskRunStatus::Paused);
    assert_eq!(steps.load(Ordering::SeqCst), 0);
    let latest = "Inspect the cancellation boundary before the retry boundary";
    let latest_source = crate::ConversationTurnRef::new(
        session.session_scope_id(),
        "latest-guidance",
        "latest-root-run",
    )?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        latest,
        &latest_source,
        &mut crate::NoopEventHandler,
    )?;
    let accepted = session.entries().len();
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &first_source,
        &mut crate::NoopEventHandler,
    )?;
    assert_eq!(
        session.entries().len(),
        accepted,
        "old source replay does not replace or reorder newer input"
    );
    assert!(
        accept_initial_continuation_guidance(
            &mut session,
            &request,
            "Overwrite later requirements",
            &first_source,
            &mut crate::NoopEventHandler
        )
        .is_err()
    );
    assert_eq!(session.entries().len(), accepted);
    let mut session = Session::load_from_store("test", "model", store)?;
    let second = Box::pin(orchestrator.run_with_initial_guidance(
        &mut session,
        request.clone(),
        opts.clone(),
        opts.clone(),
        opts.clone(),
        opts,
        8,
        None,
        &mut crate::NoopEventHandler,
        &mut crate::AutoApproveHandler,
    ))
    .await?;
    assert_eq!(second.status, TaskRunStatus::Completed);
    let inputs = inputs.lock().expect("input capture");
    assert_eq!(inputs.len(), 2);
    let prompt = inputs[1]
        .persisted_user_message
        .as_deref()
        .expect("fresh planner input");
    assert!(
        prompt.find(GUIDANCE).expect("earlier source")
            < prompt.find(latest).expect("latest source")
    );
    let guidances = initial_task_guidances(&session, &request.task_id, None)?;
    assert_eq!(guidances.len(), 2);
    assert_eq!(guidances[1].selection.source_turn, latest_source);
    assert!(
        guidances
            .iter()
            .all(|guidance| initial_guidance_consumed_by_plan(&session, guidance))
    );
    Ok(())
}

#[tokio::test]
async fn initial_guidance_retry_two_recovers_original_input_before_later_guidance() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let original = begin_participant_attempt(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        None,
        crate::AgentRole::Planner,
        None,
    )?;
    let original_input = AgentRunInput::user_with_message_id(
        planner_prompt(
            &request.objective,
            crate::TaskPlannerWorktreeAvailability::UnavailableHeadless,
        ),
        task_participant_input_message_id(&original.attempt_id),
    )
    .with_task_plan_update(TaskPlanUpdateContext {
        task_id: request.task_id.clone(),
        max_plan_steps: 8,
        max_plan_versions: crate::DEFAULT_TASK_MAX_PLAN_VERSIONS,
        worktree_availability: crate::TaskPlannerWorktreeAvailability::UnavailableHeadless,
    })
    .with_run_purpose(AgentRunPurpose::TaskPlanner(TaskPlannerContext {
        task_id: request.task_id.clone(),
        attempt_id: Some(original.attempt_id.clone()),
    }))
    .with_logical_run_id(task_participant_logical_run_id(&original.attempt_id));
    let failure: anyhow::Error = crate::TaskParticipantRetryError::new(
        1,
        format!("sha256:{}", "6".repeat(64)),
        task_participant_input_hash(&original_input)?,
        crate::TaskParticipantRetryProof::AdmissionRejectedBeforeDispatch {
            zero_output: true,
            zero_tool: true,
            zero_effect: true,
        },
        anyhow!("original planner request was not sent"),
    )?
    .into();
    assert!(schedule_control_participant_retry(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        &original,
        &failure,
    )?);
    let retry = begin_participant_attempt(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        None,
        crate::AgentRole::Planner,
        None,
    )?;
    assert_eq!(retry.ordinal, 2);
    let source = source(&session)?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &source,
        &mut crate::NoopEventHandler,
    )?;
    assert!(
        validate_scheduled_retry_input(
            &session,
            &retry,
            &AgentRunInput::user("changed new retry input")
        )
        .is_err()
    );
    let mut session = Session::load_from_store("test", "model", store)?;
    let runner = GuidanceRunner::default();
    let inputs = Arc::clone(&runner.inputs);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let opts = options(root.path());
    let result = Box::pin(orchestrator.run_with_initial_guidance(
        &mut session,
        request,
        opts.clone(),
        opts.clone(),
        opts.clone(),
        opts,
        8,
        None,
        &mut crate::NoopEventHandler,
        &mut crate::AutoApproveHandler,
    ))
    .await?;
    assert_eq!(result.status, TaskRunStatus::Completed);
    let inputs = inputs.lock().expect("input capture");
    assert_eq!(inputs.len(), 2);
    assert!(inputs[0].persisted_user_message.is_none());
    assert!(inputs[0].transient_context.is_empty());
    assert_eq!(
        inputs[0].logical_run_id(),
        Some(task_participant_logical_run_id(&retry.attempt_id).as_str())
    );
    assert!(inputs[1].task_guidance_assessment.is_some());
    assert!(
        inputs[1]
            .persisted_user_message
            .as_deref()
            .is_some_and(|text| text.contains(GUIDANCE))
    );
    Ok(())
}

#[tokio::test]
async fn initial_guidance_plan_commit_gap_settles_old_planner_before_combined_replan() -> Result<()>
{
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let original = begin_participant_attempt(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        None,
        crate::AgentRole::Planner,
        None,
    )?;
    let first_source = source(&session)?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        GUIDANCE,
        &first_source,
        &mut crate::NoopEventHandler,
    )?;
    let latest = "Also include the final cancellation result in the plan";
    let latest_source = crate::ConversationTurnRef::new(
        session.session_scope_id(),
        "latest-guidance",
        "latest-root-run",
    )?;
    accept_initial_continuation_guidance(
        &mut session,
        &request,
        latest,
        &latest_source,
        &mut crate::NoopEventHandler,
    )?;
    // The old writer committed the completed child artifact before its participant terminal.
    session.append_control(ControlEntry::TaskPlan(plan(request.task_id.clone())?))?;
    assert_eq!(
        session.task_state_projection().tasks[&request.task_id].participant_attempts
            [&original.attempt_id]
            .status,
        TaskParticipantAttemptStatus::Started
    );
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    let review = recoverable_task_guidance_review(&session, &request.task_id, None)?
        .expect("latest pending source");
    assert_eq!(review.guidance, latest);
    let runner = GuidanceRunner {
        force_replan: true,
        ..GuidanceRunner::default()
    };
    let inputs = Arc::clone(&runner.inputs);
    let steps = Arc::clone(&runner.dispatched_steps);
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(runner);
    let opts = options(root.path());
    let result = Box::pin(orchestrator.continue_run_with_conversation_guidance_review(
        &mut session,
        request.clone(),
        opts.clone(),
        opts.clone(),
        opts.clone(),
        opts,
        8,
        latest.to_owned(),
        match review.authority {
            RecoverableTaskGuidanceReviewAuthority::ContinuationSelected(selection) => *selection,
            _ => bail!("expected original selection"),
        },
        &mut crate::NoopEventHandler,
        &mut crate::AutoApproveHandler,
    ))
    .await?;
    assert_eq!(result.status, TaskRunStatus::Completed);
    assert_eq!(steps.load(Ordering::SeqCst), 1);
    let inputs = inputs.lock().expect("input capture");
    assert_eq!(
        inputs.len(),
        1,
        "the already completed physical request must not be sent again"
    );
    assert!(inputs[0].task_guidance_assessment.is_some());
    let prompt = inputs[0]
        .persisted_user_message
        .as_deref()
        .expect("combined review input");
    assert!(prompt.contains(GUIDANCE) && prompt.contains(latest));
    let completed = session.entries().iter().position(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
            if attempt.attempt_id == original.attempt_id && attempt.status == TaskParticipantAttemptStatus::Completed
    )).expect("original planner settled");
    let reviewed = session.entries().iter().position(|entry| matches!(entry,
        SessionLogEntry::Control(ControlEntry::TaskParticipantAttempt(attempt))
            if attempt.attempt_id != original.attempt_id && attempt.purpose == TaskParticipantPurpose::Planner
                && attempt.status == TaskParticipantAttemptStatus::Started
    )).expect("later review started");
    assert!(completed < reviewed);
    assert!(
        !session.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskGuidanceMaterialized(_))
        )),
        "combined input becomes a real plan, not fabricated single-source guidance"
    );
    let session = Session::load_from_store("test", "model", store)?;
    assert_eq!(
        initial_task_guidances(&session, &request.task_id, None)?.len(),
        2
    );
    assert!(recoverable_task_guidance_review(&session, &request.task_id, None)?.is_none());
    assert_eq!(
        session.task_state_projection().tasks[&request.task_id].latest_plan_version,
        Some(2)
    );
    Ok(())
}

#[test]
fn committed_plan_recovery_rejects_a_foreign_task_generation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let store = crate::JsonlSessionStore::new(root.path().join("session.jsonl"))?;
    let (mut session, request) = seeded_session(store.clone())?;
    let original = begin_participant_attempt(
        &mut session,
        &mut crate::NoopEventHandler,
        &request,
        TaskParticipantPurpose::Planner,
        None,
        None,
        crate::AgentRole::Planner,
        None,
    )?;
    // The interrupted writer committed the plan before its participant terminal: the publication
    // gap recovery repairs on the next run.
    session.append_control(ControlEntry::TaskPlan(plan(request.task_id.clone())?))?;
    let before = session.entries().len();

    let mut changed_objective = request.clone();
    changed_objective.objective = "Replace the task objective".to_owned();
    let error = reconcile_committed_initial_planner(
        &mut session,
        &changed_objective,
        &mut crate::NoopEventHandler,
    )
    .expect_err("recovery must not repair a plan for another task generation");
    assert!(error.to_string().contains("changed its Task identity"));

    let mut changed_parent = request.clone();
    changed_parent.parent_session_ref = SessionRef::new_relative("other-parent.jsonl")?;
    assert!(
        reconcile_committed_initial_planner(
            &mut session,
            &changed_parent,
            &mut crate::NoopEventHandler
        )
        .is_err()
    );
    assert_eq!(
        session.entries().len(),
        before,
        "a rejected recovery must not append settlement controls"
    );

    reconcile_committed_initial_planner(&mut session, &request, &mut crate::NoopEventHandler)?;
    let session = Session::load_from_store("test", "model", store)?;
    assert_eq!(
        session.task_state_projection().tasks[&request.task_id].participant_attempts
            [&original.attempt_id]
            .status,
        TaskParticipantAttemptStatus::Completed
    );
    Ok(())
}
