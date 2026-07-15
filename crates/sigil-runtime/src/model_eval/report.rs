use std::{collections::BTreeMap, path::PathBuf};

use anyhow::Result;
use sigil_kernel::{
    ControlEntry, EvalEvidenceKind, EvalEvidenceRef, EvalFailure, EvalFailureKind, EvalResult,
    EvalRunMetadata, EvalToolCallStatus, EvalToolCallSummary, JsonlSessionStore,
    MODEL_EVAL_REPORT_SCHEMA_VERSION, ModelEvalCostConfidence as ReportCostConfidence,
    ModelEvalExecutionStatus, ModelEvalReportCampaignV3, ModelEvalReportRecordV3, ModelEvalUsage,
    ProjectionCursor, RunStatus, Session, SessionLogEntry, ToolErrorKind, ToolExecutionStatus,
    VerificationVerdict, write_model_eval_report_v3,
};

use crate::application_run::ApplicationRunTerminalStatus;

use super::{
    ModelEvalCampaignExecution, ModelEvalCostConfidence, ModelEvalExpectedTerminal,
    ModelEvalExpectedVerification, ModelEvalRunExecution, ModelEvalRunExecutionStatus,
    sha256_digest,
};

/// Builds and writes report schema V3 for a completed model evaluation campaign.
pub fn write_model_eval_campaign_report(
    campaign: &ModelEvalCampaignExecution,
) -> Result<sigil_kernel::ModelEvalReportArtifactsV3> {
    let records = campaign
        .runs
        .iter()
        .map(build_model_eval_report_record)
        .collect::<Result<Vec<_>>>()?;
    write_model_eval_report_v3(
        &campaign.output_dir,
        &ModelEvalReportCampaignV3 {
            campaign_id: campaign.campaign_id.clone(),
            started_at_unix_ms: campaign.started_at_unix_ms,
            ended_at_unix_ms: campaign.ended_at_unix_ms,
            requested_repetitions: campaign.planned_runs,
            charged_microusd: campaign.charged_microusd,
            records,
        },
    )
}

fn build_model_eval_report_record(
    execution: &ModelEvalRunExecution,
) -> Result<ModelEvalReportRecordV3> {
    let session = load_execution_session(execution)?;
    let run_status = run_status(execution);
    let verification_verdict = execution.verification.as_ref().map_or_else(
        || {
            if execution.status == ModelEvalRunExecutionStatus::Completed {
                VerificationVerdict::Missing
            } else {
                VerificationVerdict::NotEvaluated
            }
        },
        |verification| verification.verdict,
    );
    let expected_run_statuses = execution
        .materialized_fixture
        .expected_terminal
        .iter()
        .copied()
        .map(expected_run_status)
        .collect::<Vec<_>>();
    let expected_verification_verdicts = execution
        .materialized_fixture
        .expected_verification
        .iter()
        .copied()
        .map(expected_verification_verdict)
        .collect::<Vec<_>>();
    let mut mismatch_reasons = Vec::new();
    if !expected_run_statuses.contains(&run_status) {
        mismatch_reasons.push(format!("unexpected terminal run status: {run_status:?}"));
    }
    if !expected_verification_verdicts.contains(&verification_verdict) {
        mismatch_reasons.push(format!(
            "unexpected verification verdict: {verification_verdict:?}"
        ));
    }
    if execution.safe_error.is_some() {
        mismatch_reasons.push("execution retained a harness or runtime error".to_owned());
    }

    let mut failures = execution_failures(execution, verification_verdict);
    for reason in &mismatch_reasons {
        failures.push(EvalFailure::new(EvalFailureKind::Integrity, reason));
    }
    let tool_schema_digest = tool_schema_digest(&session);
    let sandbox_backend = sandbox_backend(execution);
    let mut metadata = EvalRunMetadata {
        case_id: execution.fixture_id.clone(),
        run_id: execution.run_id.clone(),
        fixture_id: execution.fixture_id.clone(),
        repo_fixture_commit: None,
        sigil_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        provider: execution.provider.clone(),
        model: execution.model.clone(),
        model_parameters_hash: sha256_digest(
            format!(
                "max_turns={}\nmax_output_tokens={}",
                execution.materialized_fixture.max_turns,
                execution.materialized_fixture.max_output_tokens
            )
            .as_bytes(),
        ),
        tool_schema_digest,
        config_hash: execution.config_digest.clone(),
        sandbox_backend,
        os_toolchain: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        seed: None,
        provenance: Vec::new(),
        expected_outcome: None,
        expected_verification_verdict: (expected_verification_verdicts.len() == 1)
            .then_some(expected_verification_verdicts[0]),
    };
    metadata = metadata.with_provenance("RFC-0028", "R28.4");
    let mut result =
        EvalResult::from_completion(metadata, run_status, verification_verdict, failures);
    let (tool_calls, changed_files, approval_count) = session_activity(&session);
    result.tool_calls = tool_calls;
    result.changed_files = changed_files;
    result.approval_count = approval_count;
    result.session_log_path = execution
        .session_path
        .exists()
        .then(|| execution.session_path.clone());
    result.durable_stream_cursor = durable_cursor(&execution.session_path)?;

    let mut verification_receipt_ids = Vec::new();
    let mut workspace_snapshot_ids = Vec::new();
    let mut changeset_ids = Vec::new();
    if let Some(verification) = &execution.verification {
        for recorded in &verification.receipts {
            push_unique(
                &mut verification_receipt_ids,
                recorded.receipt.receipt.receipt_id.clone(),
            );
            push_unique(
                &mut workspace_snapshot_ids,
                recorded.receipt.binding.workspace_snapshot_id.clone(),
            );
            if let Some(changeset_id) = &recorded.receipt.receipt.changeset_id {
                push_unique(&mut changeset_ids, changeset_id.clone());
            }
            result.evidence.push(EvalEvidenceRef {
                kind: EvalEvidenceKind::Receipt,
                id: recorded.receipt.receipt.receipt_id.clone(),
                event_id: Some(recorded.receipt.receipt.source_event_id.clone()),
                artifact_ref: None,
            });
        }
        if let Some(snapshot_id) = &verification.current_workspace_snapshot_id {
            push_unique(&mut workspace_snapshot_ids, snapshot_id.clone());
        }
    }

    Ok(ModelEvalReportRecordV3 {
        report_schema_version: MODEL_EVAL_REPORT_SCHEMA_VERSION,
        repetition: execution.repetition,
        execution_status: report_execution_status(execution.status),
        fixture_source_digest: execution.manifest_digest.clone(),
        fixture_tree_digest: execution.tree_digest.clone(),
        isolated_config_digest: execution.isolated_config_digest.clone(),
        usage: ModelEvalUsage {
            prompt_tokens: execution.usage.prompt_tokens,
            completion_tokens: execution.usage.completion_tokens,
            cache_hit_tokens: execution.usage.cache_hit_tokens,
            cache_miss_tokens: execution.usage.cache_miss_tokens,
            reported_cost_microusd: execution.usage.total_cost_usd().and_then(usd_to_microusd),
            charged_microusd: execution.charged_microusd,
            confidence: match execution.cost_confidence {
                ModelEvalCostConfidence::Reported => ReportCostConfidence::Reported,
                ModelEvalCostConfidence::Unknown => ReportCostConfidence::Unknown,
            },
        },
        wall_time_ms: execution.wall_time.as_millis().min(u128::from(u64::MAX)) as u64,
        public_event_count: execution.public_event_count,
        expected_run_statuses,
        expected_verification_verdicts,
        acceptance_passed: mismatch_reasons.is_empty(),
        mismatch_reasons,
        verification_receipt_ids,
        workspace_snapshot_ids,
        changeset_ids,
        session_artifact_path: execution
            .session_path
            .exists()
            .then(|| execution.session_path.clone()),
        safe_error: execution.safe_error.clone(),
        result,
    })
}

fn load_execution_session(execution: &ModelEvalRunExecution) -> Result<Session> {
    if !execution.session_path.exists() {
        return Ok(Session::new(&execution.provider, &execution.model));
    }
    Session::load_from_store(
        &execution.provider,
        &execution.model,
        JsonlSessionStore::new(&execution.session_path)?,
    )
}

fn run_status(execution: &ModelEvalRunExecution) -> RunStatus {
    match execution.status {
        ModelEvalRunExecutionStatus::Completed => match execution
            .output
            .as_ref()
            .map(|output| output.terminal_status)
        {
            Some(ApplicationRunTerminalStatus::Succeeded) => RunStatus::Completed,
            Some(ApplicationRunTerminalStatus::Blocked) => RunStatus::Blocked,
            Some(ApplicationRunTerminalStatus::Interrupted) => RunStatus::Interrupted,
            None => RunStatus::Failed,
        },
        ModelEvalRunExecutionStatus::PreparationFailed
        | ModelEvalRunExecutionStatus::ExecutionFailed => RunStatus::Failed,
        ModelEvalRunExecutionStatus::TimedOut => RunStatus::Interrupted,
        ModelEvalRunExecutionStatus::BudgetSkipped
        | ModelEvalRunExecutionStatus::DeadlineSkipped => RunStatus::Blocked,
    }
}

fn expected_run_status(expected: ModelEvalExpectedTerminal) -> RunStatus {
    match expected {
        ModelEvalExpectedTerminal::Completed => RunStatus::Completed,
        ModelEvalExpectedTerminal::Blocked => RunStatus::Blocked,
        ModelEvalExpectedTerminal::Failed => RunStatus::Failed,
    }
}

fn expected_verification_verdict(expected: ModelEvalExpectedVerification) -> VerificationVerdict {
    match expected {
        ModelEvalExpectedVerification::Passed => VerificationVerdict::Passed,
        ModelEvalExpectedVerification::Stale => VerificationVerdict::Stale,
        ModelEvalExpectedVerification::Missing => VerificationVerdict::Missing,
        ModelEvalExpectedVerification::NotApplicable => VerificationVerdict::NotApplicable,
    }
}

fn execution_failures(
    execution: &ModelEvalRunExecution,
    verification_verdict: VerificationVerdict,
) -> Vec<EvalFailure> {
    let mut failures = Vec::new();
    match execution.status {
        ModelEvalRunExecutionStatus::PreparationFailed => failures.push(EvalFailure::new(
            EvalFailureKind::Harness,
            "application run preparation failed",
        )),
        ModelEvalRunExecutionStatus::ExecutionFailed => failures.push(EvalFailure::new(
            EvalFailureKind::Model,
            "application run execution failed",
        )),
        ModelEvalRunExecutionStatus::TimedOut => failures.push(EvalFailure::new(
            EvalFailureKind::Timeout,
            "application run timed out",
        )),
        ModelEvalRunExecutionStatus::BudgetSkipped
        | ModelEvalRunExecutionStatus::DeadlineSkipped => failures.push(EvalFailure::new(
            EvalFailureKind::Harness,
            "application run was skipped before provider admission",
        )),
        ModelEvalRunExecutionStatus::Completed => {}
    }
    let kind = match verification_verdict {
        VerificationVerdict::Failed => Some(EvalFailureKind::VerificationFailed),
        VerificationVerdict::Missing => Some(EvalFailureKind::VerificationMissing),
        VerificationVerdict::Stale => Some(EvalFailureKind::VerificationStale),
        VerificationVerdict::Inconclusive => Some(EvalFailureKind::VerificationInconclusive),
        _ => None,
    };
    if let Some(kind) = kind {
        failures.push(EvalFailure::new(
            kind,
            format!("verification verdict was {verification_verdict:?}"),
        ));
    }
    failures
}

fn session_activity(session: &Session) -> (Vec<EvalToolCallSummary>, Vec<PathBuf>, u32) {
    let mut terminal = BTreeMap::<String, EvalToolCallSummary>::new();
    let mut changed_files = Vec::new();
    let mut approval_count = 0_u32;
    for entry in session.entries() {
        match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.status != ToolExecutionStatus::Started =>
            {
                for path in &execution.changed_files {
                    let path = PathBuf::from(path);
                    if !changed_files.iter().any(|existing| existing == &path) {
                        changed_files.push(path);
                    }
                }
                terminal.insert(
                    execution.call_id.clone(),
                    EvalToolCallSummary {
                        tool_call_id: execution.call_id.clone(),
                        tool_name: execution.tool_name.clone(),
                        status: match execution.status {
                            ToolExecutionStatus::Completed => EvalToolCallStatus::Succeeded,
                            ToolExecutionStatus::Cancelled | ToolExecutionStatus::Interrupted => {
                                EvalToolCallStatus::Interrupted
                            }
                            ToolExecutionStatus::Failed => {
                                if execution.error.as_ref().is_some_and(|error| {
                                    matches!(
                                        error.kind,
                                        ToolErrorKind::PermissionDenied
                                            | ToolErrorKind::ApprovalDenied
                                            | ToolErrorKind::PathOutsideWorkspace
                                            | ToolErrorKind::ExternalDirectoryRequired
                                    )
                                }) {
                                    EvalToolCallStatus::Denied
                                } else {
                                    EvalToolCallStatus::Failed
                                }
                            }
                            ToolExecutionStatus::Started => unreachable!(),
                        },
                    },
                );
            }
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.user_decision.is_some() =>
            {
                approval_count = approval_count.saturating_add(1);
            }
            _ => {}
        }
    }
    (
        terminal.into_values().collect(),
        changed_files,
        approval_count,
    )
}

fn tool_schema_digest(session: &Session) -> String {
    session
        .entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
                Some(snapshot.tool_schema_fingerprint.clone())
            }
            _ => None,
        })
        .map_or_else(
            || "sha256:unknown".to_owned(),
            |digest| {
                if digest.starts_with("sha256:") {
                    digest
                } else {
                    format!("sha256:{digest}")
                }
            },
        )
}

fn sandbox_backend(execution: &ModelEvalRunExecution) -> String {
    execution
        .verification
        .as_ref()
        .and_then(|verification| verification.receipts.first())
        .and_then(|entry| entry.receipt.binding.execution_backend)
        .map_or_else(
            || "unknown".to_owned(),
            |backend| backend.as_str().to_owned(),
        )
}

fn durable_cursor(path: &PathBuf) -> Result<Option<ProjectionCursor>> {
    let records = JsonlSessionStore::read_event_records(path)?;
    Ok(records.last().map(|record| record.projection_cursor(1)))
}

fn report_execution_status(status: ModelEvalRunExecutionStatus) -> ModelEvalExecutionStatus {
    match status {
        ModelEvalRunExecutionStatus::Completed => ModelEvalExecutionStatus::Completed,
        ModelEvalRunExecutionStatus::PreparationFailed => {
            ModelEvalExecutionStatus::PreparationFailed
        }
        ModelEvalRunExecutionStatus::ExecutionFailed => ModelEvalExecutionStatus::ExecutionFailed,
        ModelEvalRunExecutionStatus::TimedOut => ModelEvalExecutionStatus::TimedOut,
        ModelEvalRunExecutionStatus::BudgetSkipped => ModelEvalExecutionStatus::BudgetSkipped,
        ModelEvalRunExecutionStatus::DeadlineSkipped => ModelEvalExecutionStatus::DeadlineSkipped,
    }
}

fn usd_to_microusd(value: f64) -> Option<u64> {
    if !value.is_finite() || value < 0.0 || value > u64::MAX as f64 / 1_000_000.0 {
        return None;
    }
    Some((value * 1_000_000.0).ceil() as u64)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}
