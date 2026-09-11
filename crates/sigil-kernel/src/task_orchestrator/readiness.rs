use super::*;
use crate::verification::VerificationExecutionPortV1;

pub(super) fn append_task_readiness<H>(
    session: &mut Session,
    handler: &mut H,
    entry: ReadinessEvaluatedEntry,
) -> Result<()>
where
    H: EventHandler + Send,
{
    append_task_control(session, handler, ControlEntry::ReadinessEvaluated(entry))
}

pub(super) async fn run_task_step_verification_checks<H>(
    session: &mut Session,
    handler: &mut H,
    verification_execution_port: Option<&dyn VerificationExecutionPortV1>,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
    readiness: &ReadinessEvaluatedEntry,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    run_task_scope_verification_checks(
        session,
        handler,
        verification_execution_port,
        &request.task_id,
        task_step_evidence_scope(&request.task_id, &step.step_id),
        options,
        readiness,
    )
    .await
}

pub(super) async fn run_task_scope_verification_checks<H>(
    session: &mut Session,
    handler: &mut H,
    verification_execution_port: Option<&dyn VerificationExecutionPortV1>,
    task_id: &TaskId,
    scope: EvidenceScope,
    options: &AgentRunOptions,
    readiness: &ReadinessEvaluatedEntry,
) -> Result<bool>
where
    H: EventHandler + Send,
{
    let check_ids = readiness
        .evaluation
        .required_actions
        .iter()
        .filter_map(|action| match action {
            RequiredAction::RunCheck { check_spec_id } => Some(check_spec_id.clone()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>();
    if check_ids.is_empty() {
        return Ok(false);
    }
    let verification_execution_port = verification_execution_port.ok_or_else(|| {
        anyhow!("verification check execution requires the managed verification execution port")
    })?;

    let projection = session.verification_state_projection();
    let step_scope = scope;
    let task_scope = EvidenceScope::Task(task_id.as_str().to_owned());
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let policy_entry = projection
        .latest_policy(&step_scope)
        .or_else(|| projection.latest_policy(&task_scope));
    let policy = policy_entry
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &step_scope, &task_scope, &workspace_scope)
        });
    let policy_hash = match policy_entry {
        Some(entry) => Some(entry.policy_hash.clone()),
        None => Some(policy.stable_hash()?),
    };
    let trust_entry = projection.workspace_trust.get(&workspace_id);
    let workspace_trust = trust_entry
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let workspace_trust_snapshot_id = trust_entry
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let scopes = [step_scope.clone(), task_scope, workspace_scope];
    for check_id in check_ids {
        let check_entry = scopes
            .iter()
            .find_map(|scope| projection.check_spec(scope, &check_id))
            .ok_or_else(|| anyhow!("missing trusted verification check spec {check_id}"))?;
        execute_task_verification_check(
            session,
            handler,
            verification_execution_port,
            TaskVerificationExecution {
                workspace_root: &options.workspace_root,
                scope: &step_scope,
                trusted_check: &check_entry.trusted_check,
                policy: &policy,
                policy_hash: policy_hash.as_deref(),
                workspace_snapshot_id: readiness.workspace_snapshot_id.as_deref(),
                workspace_trust,
                workspace_trust_snapshot_id: &workspace_trust_snapshot_id,
            },
        )
        .await?;
    }
    Ok(true)
}

/// Reruns one trusted task verification check against the current workspace.
///
/// The request is rejected before appending a queued lifecycle record when the task/step scope,
/// trusted check hash, or policy hash has drifted. The rendered workspace observation is advisory;
/// the runner captures fresh evidence for this execution.
///
/// # Errors
///
/// Returns an error when the binding is stale, the check is not currently eligible, trust is not
/// satisfied, durable lifecycle records cannot be appended, or the existing verification runner
/// cannot execute the check.
pub async fn rerun_task_verification_check<H>(
    session: &mut Session,
    handler: &mut H,
    verification_execution_port: &dyn VerificationExecutionPortV1,
    workspace_root: &Path,
    request: &TaskVerificationRerunRequest,
) -> Result<TaskVerificationRerunOutput>
where
    H: EventHandler + Send,
{
    if !request.has_exact_identity() {
        bail!("verification rerun request identity does not match its rendered binding");
    }
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&request.task_id)
        .ok_or_else(|| anyhow!("verification rerun task is no longer available"))?;
    if task.latest_plan_version != Some(request.plan_version)
        || task
            .superseded_plan_versions
            .contains(&request.plan_version)
    {
        bail!("verification task plan changed since the rerun action was rendered");
    }
    let plan = task
        .plans
        .get(&request.plan_version)
        .ok_or_else(|| anyhow!("verification rerun plan is no longer available"))?;
    if !plan
        .steps
        .iter()
        .any(|step| step.step_id == request.step_id)
    {
        bail!("verification rerun step is not part of the rendered task plan");
    }

    let projection = session.verification_state_projection();
    let step_scope = task_step_evidence_scope(&request.task_id, &request.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let workspace_id = stable_workspace_id(workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let readiness = projection
        .latest_readiness(&step_scope)
        .ok_or_else(|| anyhow!("missing current verification readiness for {step_scope:?}"))?;
    if readiness.policy_hash.as_deref() != Some(request.policy_hash.as_str()) {
        bail!("verification policy changed since the rerun action was rendered");
    }

    let policy_entry = projection
        .latest_policy(&step_scope)
        .or_else(|| projection.latest_policy(&task_scope));
    let policy = policy_entry
        .map(|entry| entry.policy.clone())
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &step_scope, &task_scope, &workspace_scope)
        });
    let current_policy_hash = match policy_entry {
        Some(entry) => entry.policy_hash.clone(),
        None => policy.stable_hash()?,
    };
    if current_policy_hash != request.policy_hash {
        bail!("verification policy changed since the rerun action was rendered");
    }

    let scopes = [step_scope.clone(), task_scope, workspace_scope];
    let check_entry = scopes
        .iter()
        .find_map(|scope| projection.check_spec(scope, &request.check_spec_id))
        .ok_or_else(|| {
            anyhow!(
                "missing trusted verification check spec {}",
                request.check_spec_id
            )
        })?;
    let check_spec = &check_entry.trusted_check.check_spec;
    if check_spec.check_spec_hash != request.check_spec_hash {
        bail!("verification check changed since the rerun action was rendered");
    }
    if !policy.required_checks.iter().any(|check| {
        check.check_spec_id == request.check_spec_id
            && check.check_spec_hash == request.check_spec_hash
    }) {
        bail!("verification check is not required by the current policy");
    }

    let latest_run = latest_task_check_run(
        session,
        &step_scope,
        &request.check_spec_id,
        &request.check_spec_hash,
    );
    if latest_run.is_some_and(|run| {
        matches!(
            run.status,
            VerificationCheckRunStatus::Queued | VerificationCheckRunStatus::Running
        )
    }) {
        bail!("verification check is already queued or running");
    }
    let action_allows_run =
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| match action {
                RequiredAction::RunCheck { check_spec_id }
                | RequiredAction::ReRunNonWritingCheck { check_spec_id } => {
                    check_spec_id == &request.check_spec_id
                }
                _ => false,
            });
    let retryable_terminal = latest_run.is_some_and(|run| {
        matches!(
            run.status,
            VerificationCheckRunStatus::Failed
                | VerificationCheckRunStatus::Inconclusive
                | VerificationCheckRunStatus::Succeeded
        )
    });
    if !action_allows_run && !retryable_terminal {
        bail!("verification check is not eligible for rerun in the current task scope");
    }

    let current_snapshot = build_workspace_snapshot(
        workspace_root,
        workspace_id.clone(),
        &policy.verification_scope,
        0,
    )?;

    let trust_entry = projection.workspace_trust.get(&workspace_id);
    let workspace_trust = trust_entry
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let workspace_trust_snapshot_id = trust_entry
        .map(|entry| entry.workspace_trust_snapshot_id.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    execute_task_verification_check(
        session,
        handler,
        verification_execution_port,
        TaskVerificationExecution {
            workspace_root,
            scope: &step_scope,
            trusted_check: &check_entry.trusted_check,
            policy: &policy,
            policy_hash: Some(&request.policy_hash),
            workspace_snapshot_id: current_snapshot.workspace_snapshot_id.as_deref(),
            workspace_trust,
            workspace_trust_snapshot_id: &workspace_trust_snapshot_id,
        },
    )
    .await
}

struct TaskVerificationExecution<'a> {
    workspace_root: &'a Path,
    scope: &'a EvidenceScope,
    trusted_check: &'a TrustedCheckSpec,
    policy: &'a VerificationPolicy,
    policy_hash: Option<&'a str>,
    workspace_snapshot_id: Option<&'a str>,
    workspace_trust: WorkspaceTrust,
    workspace_trust_snapshot_id: &'a str,
}

async fn execute_task_verification_check<H>(
    session: &mut Session,
    handler: &mut H,
    verification_execution_port: &dyn VerificationExecutionPortV1,
    execution: TaskVerificationExecution<'_>,
) -> Result<TaskVerificationRerunOutput>
where
    H: EventHandler + Send,
{
    let TaskVerificationExecution {
        workspace_root,
        scope,
        trusted_check,
        policy,
        policy_hash,
        workspace_snapshot_id,
        workspace_trust,
        workspace_trust_snapshot_id,
    } = execution;
    let check_spec = &trusted_check.check_spec;
    let run_id = verification_check_run_id(
        scope,
        check_spec,
        policy_hash,
        workspace_snapshot_id,
        session.next_stream_sequence_hint()?,
    )?;
    let queued = VerificationCheckRunEntry::new(
        run_id.clone(),
        scope.clone(),
        check_spec,
        VerificationCheckRunStatus::Queued,
    )
    .with_timeout_ms(policy.timeout_ms);
    append_task_control(session, handler, ControlEntry::VerificationCheckRun(queued))?;
    let running = VerificationCheckRunEntry::new(
        run_id.clone(),
        scope.clone(),
        check_spec,
        VerificationCheckRunStatus::Running,
    )
    .with_timeout_ms(policy.timeout_ms);
    append_task_control(
        session,
        handler,
        ControlEntry::VerificationCheckRun(running),
    )?;
    let verification_evidence = match run_verification_check_with_evidence(
        session,
        verification_execution_port,
        VerificationCheckRunRequest {
            workspace_root: workspace_root.to_path_buf(),
            scope: scope.clone(),
            trusted_check: trusted_check.clone(),
            policy: policy.clone(),
            policy_hash: policy_hash.map(str::to_owned),
            workspace_trust,
            workspace_trust_snapshot_id: workspace_trust_snapshot_id.to_owned(),
            workspace_trust_approval_event_id: None,
            workspace_trust_sandbox_decision_id: None,
        },
    )
    .await
    {
        Ok(recorded) => recorded,
        Err(error) => {
            let error_summary = error.to_string();
            let check_run = VerificationCheckRunEntry::new(
                run_id,
                scope.clone(),
                check_spec,
                VerificationCheckRunStatus::Errored,
            )
            .with_timeout_ms(policy.timeout_ms)
            .with_error(error_summary);
            append_task_control(
                session,
                handler,
                ControlEntry::VerificationCheckRun(check_run.clone()),
            )?;
            append_verification_evidence_records(session, handler, &check_run, None, None, None)?;
            return Err(error);
        }
    };
    let verification = verification_evidence.recorded;
    let command_event_id = verification_evidence.command_event_id;
    let receipt = verification.receipt.clone();
    let receipt_event = append_task_control_with_event(
        session,
        handler,
        ControlEntry::VerificationRecorded(verification.clone()),
    )?;
    let check_run = VerificationCheckRunEntry::new(
        run_id,
        scope.clone(),
        check_spec,
        VerificationCheckRunStatus::Running,
    )
    .with_timeout_ms(policy.timeout_ms)
    .with_terminal_receipt(&receipt);
    append_task_control(
        session,
        handler,
        ControlEntry::VerificationCheckRun(check_run.clone()),
    )?;
    append_verification_evidence_records(
        session,
        handler,
        &check_run,
        Some(&receipt),
        receipt_event.as_ref(),
        command_event_id.as_deref(),
    )?;
    Ok(TaskVerificationRerunOutput {
        check_run,
        verification,
    })
}

fn append_verification_evidence_records<H>(
    session: &mut Session,
    handler: &mut H,
    check_run: &VerificationCheckRunEntry,
    receipt: Option<&VerificationReceipt>,
    receipt_event: Option<&StoredEvent>,
    command_event_id: Option<&str>,
) -> Result<()>
where
    H: EventHandler + Send,
{
    let needs_stream_records =
        receipt.is_some_and(|receipt| receipt.receipt.changeset_id.is_some());
    let records = if needs_stream_records {
        verification_stream_records(session)?
    } else {
        Vec::new()
    };
    if let Some(receipt) = receipt {
        let link = verification_receipt_link_from_records(receipt, receipt_event, &records)?;
        append_task_control(
            session,
            handler,
            ControlEntry::VerificationReceiptLinkRecorded(link),
        )?;
    }
    let Some(locator) =
        verification_failure_locator_from_records(check_run, receipt, command_event_id, &records)?
    else {
        return Ok(());
    };
    append_task_control(
        session,
        handler,
        ControlEntry::VerificationFailureLocatorRecorded(locator),
    )
}

fn verification_stream_records(session: &Session) -> Result<Vec<SessionStreamRecord>> {
    session
        .durable_store()
        .map(|store| store.read_event_records_coordinated())
        .transpose()
        .map(Option::unwrap_or_default)
}

fn latest_task_check_run<'a>(
    session: &'a Session,
    scope: &EvidenceScope,
    check_spec_id: &str,
    check_spec_hash: &str,
) -> Option<&'a VerificationCheckRunEntry> {
    session.entries().iter().rev().find_map(|entry| {
        let SessionLogEntry::Control(ControlEntry::VerificationCheckRun(run)) = entry else {
            return None;
        };
        (run.scope == *scope
            && run.check_spec_id == check_spec_id
            && run.check_spec_hash == check_spec_hash)
            .then_some(run)
    })
}

pub(super) fn task_step_auto_run_policy(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
) -> Result<VerificationAutoRunPolicy> {
    let projection = session.verification_state_projection();
    let step_scope = task_step_evidence_scope(&request.task_id, &step.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id);
    Ok(projection
        .latest_policy(&step_scope)
        .or_else(|| projection.latest_policy(&task_scope))
        .map(|entry| entry.policy.auto_run)
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &step_scope, &task_scope, &workspace_scope)
                .auto_run
        }))
}

pub(super) fn task_step_readiness(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    status: TaskStepStatus,
    output: &StepRunOutput,
    options: &AgentRunOptions,
) -> Result<ReadinessEvaluatedEntry> {
    let scope = task_step_evidence_scope(&request.task_id, &step.step_id);
    let task_scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
    let workspace_id = stable_workspace_id(&options.workspace_root)?;
    let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
    let projection = session.verification_state_projection();
    let policy = projection
        .latest_policy(&scope)
        .map(|entry| entry.policy.clone())
        .or_else(|| {
            projection
                .latest_policy(&task_scope)
                .map(|entry| entry.policy.clone())
        })
        .unwrap_or_else(|| {
            task_step_default_policy(&projection, &scope, &task_scope, &workspace_scope)
        });
    let baseline_policy_hash = policy.stable_hash()?;
    let latest_successful_verification_sequence = latest_relevant_successful_verification_sequence(
        &projection,
        &[scope.clone(), task_scope.clone()],
        &policy,
        &baseline_policy_hash,
    );
    let durable_mutation_evidence_result = durable_workspace_mutation_evidence(
        session,
        &request.task_id,
        &VerificationScope::all_tracked(task_step_verification_scope_hash()),
        &output.outcome.tool_call_ids,
        latest_successful_verification_sequence,
    );
    let source_stream_sequence = session.next_stream_sequence_hint().unwrap_or(1);
    let mut durable_mutation_evidence = match durable_mutation_evidence_result {
        Ok(evidence) => evidence,
        Err(_) => vec![durable_mutation_replay_failed_evidence(
            request,
            step,
            task_step_verification_scope_hash(),
            source_stream_sequence,
        )],
    };
    let step_has_workspace_mutation =
        !output.outcome.changed_files.is_empty() || !durable_mutation_evidence.is_empty();
    let policy_hash = policy.stable_hash()?;
    let mut input = ReadinessInput::new_run(run_status_from_step_status(status), policy);
    input.workspace_trust = projection
        .workspace_trust
        .get(&workspace_id)
        .map(|entry| entry.trust)
        .unwrap_or(WorkspaceTrust::Unknown);
    let trust_ids = check_scope_trust_ids(
        &projection,
        &[scope.clone(), task_scope.clone(), workspace_scope.clone()],
    );
    input.workspace_trust_approval_event_id = trust_ids.approval_event_id;
    input.workspace_trust_sandbox_decision_id = trust_ids.sandbox_decision_id;
    if step_has_workspace_mutation {
        let snapshot_event_id = format!(
            "readiness-snapshot:{}:{}",
            request.task_id.as_str(),
            step.step_id.as_str()
        );
        let snapshot = build_workspace_snapshot_for_event(
            &options.workspace_root,
            workspace_id,
            &input.policy.verification_scope,
            0,
            snapshot_event_id,
            source_stream_sequence,
        )?;
        input.current_workspace_snapshot_id = snapshot.workspace_snapshot_id;
        input.workspace_knowledge = snapshot.workspace_knowledge;
        if let Some(evidence) = snapshot.unknown_dirty_evidence {
            input.mutations.push(evidence);
        }
    }
    if status == TaskStepStatus::Completed && !output.outcome.tool_errors.is_empty() {
        input.recovered_tool_error_event_ids = output
            .outcome
            .tool_errors
            .iter()
            .enumerate()
            .map(|(index, _)| {
                format!(
                    "task-step-recovered-tool-error:{}:{}:{}:{}",
                    request.task_id.as_str(),
                    step.step_id.as_str(),
                    source_stream_sequence,
                    index
                )
            })
            .collect();
    }
    input.verification_receipts = relevant_verification_receipts(
        &projection,
        &[scope.clone(), task_scope.clone()],
        &input.policy,
        &policy_hash,
    );
    if step_has_workspace_mutation {
        if durable_mutation_evidence.is_empty() {
            durable_mutation_evidence.push(changed_files_mutation_evidence(
                request,
                step,
                &input.policy.verification_scope.scope_hash,
                input.current_workspace_snapshot_id.as_deref(),
                1,
            ));
        }
        input.mutations.extend(durable_mutation_evidence);
    }
    if input
        .mutations
        .iter()
        .any(|mutation| mutation.unknown_dirty)
    {
        input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
    } else if step_has_workspace_mutation && !input.workspace_knowledge.is_unknown_dirty() {
        let latest_mutation_sequence = input
            .mutations
            .iter()
            .map(|mutation| mutation.recorded_at_stream_sequence)
            .max()
            .unwrap_or(source_stream_sequence);
        input.workspace_knowledge = WorkspaceKnowledge::Dirty(latest_mutation_sequence);
    }
    let evaluation = evaluate_readiness(&input);
    Ok(ReadinessEvaluatedEntry {
        scope,
        evaluation,
        policy_hash: Some(policy_hash),
        workspace_snapshot_id: input.current_workspace_snapshot_id,
    })
}

pub(super) async fn task_step_readiness_nonblocking(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    status: TaskStepStatus,
    output: &StepRunOutput,
    options: &AgentRunOptions,
) -> Result<ReadinessEvaluatedEntry> {
    let mut session_snapshot = Session::from_entries(
        session.provider_name().to_owned(),
        session.model_name().to_owned(),
        session.entries().to_vec(),
    );
    if let Some(store) = session.durable_store() {
        session_snapshot = session_snapshot.with_store(store);
    }
    let request = request.clone();
    let step = step.clone();
    let output = output.clone();
    let options = options.clone();
    tokio::task::spawn_blocking(move || {
        task_step_readiness(
            &session_snapshot,
            &request,
            &step,
            status,
            &output,
            &options,
        )
    })
    .await
    .map_err(|error| anyhow!("task step readiness worker failed: {error}"))?
}

pub(super) async fn task_step_failure_readiness_nonblocking(
    session: &Session,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
) -> Result<ReadinessEvaluatedEntry> {
    let output = StepRunOutput {
        final_text: String::new(),
        outcome: AgentRunOutcome::default(),
        final_answer_ref: None,
        artifact_refs: Vec::new(),
        changeset_proposal: None,
        isolated_parent_snapshot_id: None,
    };
    task_step_readiness_nonblocking(
        session,
        request,
        step,
        TaskStepStatus::Failed,
        &output,
        options,
    )
    .await
}

pub(crate) fn task_step_evidence_scope(task_id: &TaskId, step_id: &TaskStepId) -> EvidenceScope {
    EvidenceScope::Step(format!("{}:{}", task_id.as_str(), step_id.as_str()))
}

pub(super) fn task_step_verification_scope_hash() -> &'static str {
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH
}

pub(super) fn task_step_default_policy(
    projection: &crate::VerificationStateProjection,
    step_scope: &EvidenceScope,
    task_scope: &EvidenceScope,
    workspace_scope: &EvidenceScope,
) -> VerificationPolicy {
    let check_entries = projection
        .check_specs_for_scopes(&[
            step_scope.clone(),
            task_scope.clone(),
            workspace_scope.clone(),
        ])
        .into_iter()
        .collect::<Vec<_>>();
    let checks = check_entries
        .iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect::<Vec<_>>();
    if checks.is_empty() {
        return VerificationPolicy::no_checks_required(task_step_verification_scope_hash());
    }
    let workspace_trust_requirement = check_entries_workspace_trust_requirement(&check_entries);
    VerificationPolicy {
        required_checks: checks,
        completion_criteria: CompletionCriteria::AllRequiredChecks,
        verification_scope: VerificationScope::all_tracked(task_step_verification_scope_hash()),
        sandbox_profile: crate::SandboxProfileRequirement::None,
        workspace_trust_requirement,
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: crate::VerificationAutoRunPolicy::Manual,
    }
}

struct CheckScopeTrustIds {
    approval_event_id: Option<String>,
    sandbox_decision_id: Option<String>,
}

fn check_scope_trust_ids(
    projection: &crate::VerificationStateProjection,
    scopes: &[EvidenceScope],
) -> CheckScopeTrustIds {
    let mut approval_event_id = None;
    let mut sandbox_decision_id = None;
    for entry in projection.check_specs_for_scopes(scopes) {
        approval_event_id =
            approval_event_id.or_else(|| entry.trusted_check.approval_event_id.clone());
        sandbox_decision_id =
            sandbox_decision_id.or_else(|| entry.trusted_check.sandbox_decision_id.clone());
    }
    CheckScopeTrustIds {
        approval_event_id,
        sandbox_decision_id,
    }
}

pub(super) fn check_entries_workspace_trust_requirement(
    check_entries: &[&crate::CheckSpecRecordedEntry],
) -> crate::WorkspaceTrustRequirement {
    if check_entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::WorkspaceTrusted { .. }
        )
    }) {
        return crate::WorkspaceTrustRequirement::Trusted;
    }
    if check_entries.iter().any(|entry| {
        matches!(
            entry.trusted_check.promoted_by,
            CheckPromotion::UserApproved { .. } | CheckPromotion::Sandboxed { .. }
        )
    }) {
        return crate::WorkspaceTrustRequirement::ApprovalOrSandbox;
    }
    crate::WorkspaceTrustRequirement::None
}

pub(super) fn relevant_verification_receipts(
    projection: &crate::VerificationStateProjection,
    scopes: &[EvidenceScope],
    policy: &VerificationPolicy,
    policy_hash: &str,
) -> Vec<VerificationReceipt> {
    projection
        .receipts
        .values()
        .filter(|entry| {
            scopes
                .iter()
                .any(|scope| scope == &entry.receipt.receipt.scope)
        })
        .filter(|entry| entry.receipt.receipt.policy_hash.as_deref() == Some(policy_hash))
        .filter(|entry| {
            entry.receipt.binding.verification_scope_hash == policy.verification_scope.scope_hash
        })
        .filter(|entry| {
            policy.required_checks.iter().any(|check| {
                check.check_spec_id == entry.receipt.check_spec_id
                    && check.check_spec_hash == entry.receipt.binding.check_spec_hash
            })
        })
        .map(|entry| entry.receipt.clone())
        .collect()
}

pub(super) fn latest_relevant_successful_verification_sequence(
    projection: &crate::VerificationStateProjection,
    scopes: &[EvidenceScope],
    policy: &VerificationPolicy,
    policy_hash: &str,
) -> u64 {
    relevant_verification_receipts(projection, scopes, policy, policy_hash)
        .into_iter()
        .filter(|receipt| receipt.check_status == crate::ReceiptStatus::Succeeded)
        .map(|receipt| receipt.receipt.recorded_at_stream_sequence)
        .max()
        .unwrap_or(0)
}

/// Evaluates the original Task scope; a direct task never needs a synthetic plan or step.
pub(super) async fn direct_task_completion_readiness(
    session: &Session,
    request: &SequentialTaskRequest,
    outcome: &AgentRunOutcome,
    options: &AgentRunOptions,
) -> Result<(
    ReadinessEvaluatedEntry,
    VerificationAutoRunPolicy,
    Option<String>,
)> {
    let mut snapshot = Session::from_entries(
        session.provider_name().to_owned(),
        session.model_name().to_owned(),
        session.entries().to_vec(),
    );
    if let Some(store) = session.durable_store() {
        snapshot = snapshot.with_store(store);
    }
    let request = request.clone();
    let outcome = outcome.clone();
    let options = options.clone();
    tokio::task::spawn_blocking(move || {
        let effect_blocker = direct_task_effect_blocker(&snapshot, &request.task_id, &outcome)?;
        let scope = EvidenceScope::Task(request.task_id.as_str().to_owned());
        let workspace_id = stable_workspace_id(&options.workspace_root)?;
        let workspace_scope = EvidenceScope::Workspace(workspace_id.clone());
        let projection = snapshot.verification_state_projection();
        let policy = projection
            .latest_policy(&scope)
            .map(|entry| entry.policy.clone())
            .unwrap_or_else(|| {
                task_step_default_policy(&projection, &scope, &scope, &workspace_scope)
            });
        let auto_run = policy.auto_run;
        let policy_hash = policy.stable_hash()?;
        let last_verified = latest_relevant_successful_verification_sequence(
            &projection,
            std::slice::from_ref(&scope),
            &policy,
            &policy_hash,
        );
        let mutations = durable_workspace_mutation_evidence(
            &snapshot,
            &request.task_id,
            &policy.verification_scope,
            &outcome.tool_call_ids,
            last_verified,
        )?;
        let has_mutation = !outcome.changed_files.is_empty() || !mutations.is_empty();
        let mut input = ReadinessInput::new_run(RunStatus::Completed, policy);
        input.workspace_trust = projection
            .workspace_trust
            .get(&workspace_id)
            .map_or(WorkspaceTrust::Unknown, |entry| entry.trust);
        let trust_ids = check_scope_trust_ids(&projection, &[scope.clone(), workspace_scope]);
        input.workspace_trust_approval_event_id = trust_ids.approval_event_id;
        input.workspace_trust_sandbox_decision_id = trust_ids.sandbox_decision_id;
        if has_mutation || !input.policy.required_checks.is_empty() {
            let workspace = build_workspace_snapshot_for_event(
                &options.workspace_root,
                workspace_id,
                &input.policy.verification_scope,
                0,
                format!("direct-readiness:{}", request.task_id.as_str()),
                snapshot.next_stream_sequence_hint().unwrap_or(1),
            )?;
            input.current_workspace_snapshot_id = workspace.workspace_snapshot_id;
            input.workspace_knowledge = workspace.workspace_knowledge;
            if let Some(evidence) = workspace.unknown_dirty_evidence {
                input.mutations.push(evidence);
            }
        }
        input.mutations.extend(mutations);
        // This outcome fallback invalidates verification conservatively; it does not certify
        // a durable mutation or authorize effect settlement.
        if has_mutation && input.mutations.is_empty() {
            input.mutations.push(WorkspaceMutationEvidence {
                event_id: format!("direct-changed-files:{}", request.task_id.as_str()),
                source_event_type: "task_direct_changed_files".to_owned(),
                source_label: None,
                recovery_hint: None,
                scope_hash: input.policy.verification_scope.scope_hash.clone(),
                recorded_at_stream_sequence: 1,
                from_workspace_snapshot_id: input.current_workspace_snapshot_id.clone(),
                to_workspace_snapshot_id: None,
                tool_effect: crate::ToolEffect::WorkspaceWrite,
                unknown_dirty: false,
            });
        }
        if input
            .mutations
            .iter()
            .any(|mutation| mutation.unknown_dirty)
        {
            input.workspace_knowledge = WorkspaceKnowledge::UnknownDirty;
        }
        input.verification_receipts = relevant_verification_receipts(
            &projection,
            std::slice::from_ref(&scope),
            &input.policy,
            &policy_hash,
        );
        Ok((
            ReadinessEvaluatedEntry {
                scope,
                evaluation: evaluate_readiness(&input),
                policy_hash: Some(policy_hash),
                workspace_snapshot_id: input.current_workspace_snapshot_id,
            },
            auto_run,
            effect_blocker,
        ))
    })
    .await
    .map_err(|error| anyhow!("direct Task readiness worker failed: {error}"))?
}

fn direct_task_effect_blocker(
    session: &Session,
    task_id: &TaskId,
    outcome: &AgentRunOutcome,
) -> Result<Option<String>> {
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(task_id)
        .ok_or_else(|| anyhow!("direct completion Task authority is unavailable"))?;
    let mut run_ids = BTreeSet::new();
    for attempt in task.direct_execution_attempts.values() {
        let run_id = crate::task_direct_execution_logical_run_id(&attempt.attempt_id);
        run_ids.extend(session.recorded_evidence_run_ids(&run_id)?);
        run_ids.insert(run_id);
    }
    if let Some(effects) = session.try_effect_reconciliation_projection_from_durable()? {
        for required in effects.unsettled() {
            if required.task_id.as_deref() == Some(task_id.as_str())
                || required
                    .logical_run_id
                    .as_ref()
                    .is_some_and(|id| run_ids.contains(id))
                || outcome.tool_call_ids.contains(&required.effect_id)
            {
                return Ok(Some(format!(
                    "effect reconciliation required: {}",
                    required.reconciliation_id
                )));
            }
        }
    }
    if let Some(blockers) = session.try_recovery_blocker_projection_from_durable()? {
        for blocker in blockers.active() {
            let applies = match &blocker.scope {
                crate::FailureScopeV1::Task { task_id: id }
                | crate::FailureScopeV1::Step { task_id: id, .. } => id == task_id,
                crate::FailureScopeV1::ToolEffect {
                    task_id: id,
                    effect_id,
                    ..
                } => id.as_ref() == Some(task_id) || outcome.tool_call_ids.contains(effect_id),
                crate::FailureScopeV1::Run { logical_run_id } => run_ids.contains(logical_run_id),
                _ => false,
            };
            if applies {
                return Ok(Some(format!(
                    "recovery blocker unresolved: {}",
                    blocker.blocker_id
                )));
            }
        }
    }
    Ok(None)
}
