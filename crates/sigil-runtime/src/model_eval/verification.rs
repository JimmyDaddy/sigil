use std::{fs, path::Path, time::Instant};

use anyhow::{Context, Result, anyhow, bail};
use sigil_kernel::verification::VerificationExecutionPortV1;
use sigil_kernel::{
    CheckCommand, CheckDiscoverySource, CheckPromotion, CheckSpec, CheckSpecRecordedEntry,
    CompletionCriteria, ControlEntry, EvidenceScope, ExecutionReceipt, ExecutionRequest,
    JsonlSessionStore, ReceiptStatus, SandboxProfileRequirement, Session, ToolEffect,
    TrustedCheckSpec, VerificationAutoRunPolicy, VerificationCheckRunRequest, VerificationPolicy,
    VerificationPolicyChangedEntry, VerificationRecordedEntry, VerificationScope,
    VerificationVerdict, WorkspaceTrust, WorkspaceTrustRequirement, build_workspace_snapshot,
    run_verification_check, stable_workspace_id, write_file_with_mutation,
};

use super::{MaterializedModelEvalFixture, ModelEvalPostRunMutation, sha256_digest};

/// Durable verification evidence observed after one provider-backed repetition.
#[derive(Debug, Clone)]
pub struct ModelEvalVerificationExecution {
    pub verdict: VerificationVerdict,
    pub receipts: Vec<VerificationRecordedEntry>,
    pub current_workspace_snapshot_id: Option<String>,
    pub post_run_mutation_recorded: bool,
}

/// Runs committed fixture checks through the configured production execution backend.
pub async fn verify_model_eval_run(
    fixture: &MaterializedModelEvalFixture,
    config_path: &Path,
    session_path: &Path,
    provider: &str,
    model: &str,
    run_id: &str,
) -> Result<ModelEvalVerificationExecution> {
    verify_model_eval_run_with_deadline(
        fixture,
        config_path,
        session_path,
        provider,
        model,
        run_id,
        None,
    )
    .await
}

pub(super) async fn verify_model_eval_run_with_deadline(
    fixture: &MaterializedModelEvalFixture,
    config_path: &Path,
    session_path: &Path,
    provider: &str,
    model: &str,
    run_id: &str,
    deadline: Option<Instant>,
) -> Result<ModelEvalVerificationExecution> {
    if fixture.checks.is_empty() {
        return Ok(ModelEvalVerificationExecution {
            verdict: VerificationVerdict::NotApplicable,
            receipts: Vec::new(),
            current_workspace_snapshot_id: None,
            post_run_mutation_recorded: false,
        });
    }

    let fixture = fixture.clone();
    let config_path = config_path.to_path_buf();
    let session_path = session_path.to_path_buf();
    let provider = provider.to_owned();
    let model = model.to_owned();
    let run_id = run_id.to_owned();
    let (
        fixture,
        mut session,
        execution_port,
        scope,
        verification_scope,
        trust_snapshot_id,
        trusted_checks,
        policy,
        policy_hash,
    ) = tokio::task::spawn_blocking(move || {
        let boot = crate::r71_authority_composition::boot_current_schema(
            &config_path,
            &fixture.workspace_root,
        )
        .map_err(|error| anyhow!("model-eval authority boot failed: {error}"))?;
        let execution_port = std::sync::Arc::clone(&boot.composition().command_execution);
        let store = JsonlSessionStore::new(&session_path)?;
        let mut session = Session::load_from_store(&provider, &model, store)?;
        let scope = EvidenceScope::Run(run_id.to_owned());
        let verification_scope = model_eval_verification_scope(&fixture)?;
        let trust_snapshot_id = format!("model-eval-fixture:{}", fixture.manifest_digest);
        let trusted_checks = fixture
            .checks
            .iter()
            .map(|check| {
                let (command, args) = check
                    .command
                    .split_first()
                    .ok_or_else(|| anyhow!("model eval verification command is empty"))?;
                let check_spec = CheckSpec::new(
                    check.id.clone(),
                    CheckCommand {
                        command: command.clone(),
                        args: args.to_vec(),
                        cwd: None,
                    },
                    ToolEffect::ReadOnly,
                    verification_scope.scope_hash.clone(),
                );
                Ok(TrustedCheckSpec {
                    check_spec,
                    source: CheckDiscoverySource::UserExplicitConfig,
                    workspace_trust_snapshot_id: trust_snapshot_id.clone(),
                    promoted_by: CheckPromotion::ExplicitUserConfig {
                        config_event_id: format!("model-eval-fixture:{}", fixture.manifest_digest),
                    },
                    approval_event_id: None,
                    sandbox_decision_id: None,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        for trusted_check in &trusted_checks {
            session.append_control(ControlEntry::CheckSpecRecorded(
                CheckSpecRecordedEntry::new(
                    scope.clone(),
                    trusted_check.clone(),
                    format!("model-eval-fixture:{}", fixture.manifest_digest),
                ),
            ))?;
        }

        let policy = VerificationPolicy {
            required_checks: trusted_checks
                .iter()
                .map(|trusted| trusted.check_spec.clone())
                .collect(),
            completion_criteria: CompletionCriteria::AllRequiredChecks,
            verification_scope: verification_scope.clone(),
            sandbox_profile: SandboxProfileRequirement::None,
            workspace_trust_requirement: WorkspaceTrustRequirement::None,
            allow_unverified_completion: false,
            timeout_ms: fixture.checks.iter().map(|check| check.timeout_ms).min(),
            auto_run: VerificationAutoRunPolicy::Manual,
        };
        let policy_entry = VerificationPolicyChangedEntry::new(
            scope.clone(),
            policy.clone(),
            format!("model-eval-policy:{run_id}"),
        )?;
        let policy_hash = policy_entry.policy_hash.clone();
        session.append_control(ControlEntry::VerificationPolicyChanged(policy_entry))?;

        Ok::<_, anyhow::Error>((
            fixture,
            session,
            execution_port,
            scope,
            verification_scope,
            trust_snapshot_id,
            trusted_checks,
            policy,
            policy_hash,
        ))
    })
    .await
    .context("model eval verification preparation worker failed")??;
    let deadline_port = DeadlineVerificationPort {
        port: execution_port.as_ref(),
        deadline,
    };

    let mut receipts = Vec::with_capacity(trusted_checks.len());
    for trusted_check in &trusted_checks {
        let recorded = run_verification_check(
            &mut session,
            &deadline_port,
            VerificationCheckRunRequest {
                workspace_root: fixture.workspace_root.clone(),
                scope: scope.clone(),
                trusted_check: trusted_check.clone(),
                policy: policy.clone(),
                policy_hash: Some(policy_hash.clone()),
                workspace_trust: WorkspaceTrust::Unknown,
                workspace_trust_snapshot_id: trust_snapshot_id.clone(),
                workspace_trust_approval_event_id: None,
                workspace_trust_sandbox_decision_id: None,
            },
        )
        .await?;
        session.append_control(ControlEntry::VerificationRecorded(recorded.clone()))?;
        receipts.push(recorded);
    }

    let run_id = match &scope {
        EvidenceScope::Run(id) => id.clone(),
        _ => unreachable!("model eval binds verification to its run"),
    };
    tokio::task::spawn_blocking(move || {
        let post_run_mutation_recorded = if let Some(mutation) = &fixture.post_run_mutation {
            apply_post_run_mutation(&session, &fixture, mutation, &run_id)?;
            true
        } else {
            false
        };
        let workspace_id = stable_workspace_id(&fixture.workspace_root)?;
        let snapshot = build_workspace_snapshot(
            &fixture.workspace_root,
            workspace_id,
            &verification_scope,
            0,
        )?;
        let verdict = verification_verdict(
            &receipts,
            &trusted_checks,
            &policy,
            snapshot.workspace_snapshot_id.as_ref(),
        );

        Ok(ModelEvalVerificationExecution {
            verdict,
            receipts,
            current_workspace_snapshot_id: snapshot.workspace_snapshot_id,
            post_run_mutation_recorded,
        })
    })
    .await
    .context("model eval verification result worker failed")?
}

// A deadline narrows the existing process owner's finite timeout. We always await its
// actual receipt instead of dropping the verification future and detaching a running check.
struct DeadlineVerificationPort<'a> {
    port: &'a dyn VerificationExecutionPortV1,
    deadline: Option<Instant>,
}

#[async_trait::async_trait]
impl VerificationExecutionPortV1 for DeadlineVerificationPort<'_> {
    async fn execute_check(&self, mut request: ExecutionRequest) -> Result<ExecutionReceipt> {
        if let Some(deadline) = self.deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("model eval deadline reached before verification dispatch");
            }
            let remaining_ms = remaining.as_millis().clamp(1, u128::from(u64::MAX)) as u64;
            request.timeout_ms = Some(
                request
                    .timeout_millis()
                    .unwrap_or(remaining_ms)
                    .min(remaining_ms),
            );
        }
        self.port.execute_check(request).await
    }
}

fn model_eval_verification_scope(
    fixture: &MaterializedModelEvalFixture,
) -> Result<VerificationScope> {
    let include = fixture
        .fixture_files
        .iter()
        .map(|path| {
            path.to_str()
                .map(str::to_owned)
                .context("model eval fixture path is not valid UTF-8")
        })
        .collect::<Result<Vec<_>>>()?;
    let scope_hash = sha256_digest(format!("model_eval_v1\n{}", include.join("\n")).as_bytes());
    let mut scope = VerificationScope::all_tracked(scope_hash);
    scope.include = include;
    scope.tracked_files_only = false;
    Ok(scope)
}

fn apply_post_run_mutation(
    session: &Session,
    fixture: &MaterializedModelEvalFixture,
    mutation: &ModelEvalPostRunMutation,
    run_id: &str,
) -> Result<()> {
    let absolute_path = fixture.workspace_root.join(&mutation.path);
    let content = fs::read_to_string(&absolute_path)
        .with_context(|| format!("failed to read {}", absolute_path.display()))?;
    if content.matches(&mutation.old_text).count() != 1 {
        bail!(
            "model eval post-run mutation expected exactly one match in {}",
            mutation.path.display()
        );
    }
    let updated = content.replacen(&mutation.old_text, &mutation.new_text, 1);
    let recorder = session
        .mutation_event_recorder()
        .context("model eval post-run mutation requires a durable session")?;
    write_file_with_mutation(
        Some(&recorder),
        &fixture.workspace_root,
        &format!("model-eval-post-run:{run_id}"),
        mutation.path.clone(),
        absolute_path,
        updated.as_bytes(),
    )?;
    Ok(())
}

fn verification_verdict(
    receipts: &[VerificationRecordedEntry],
    trusted_checks: &[TrustedCheckSpec],
    policy: &VerificationPolicy,
    current_snapshot_id: Option<&String>,
) -> VerificationVerdict {
    if receipts.len() != trusted_checks.len() || receipts.is_empty() {
        return VerificationVerdict::Missing;
    }
    if receipts
        .iter()
        .any(|entry| entry.receipt.check_status == ReceiptStatus::Failed)
    {
        return VerificationVerdict::Failed;
    }
    if receipts.iter().any(|entry| {
        matches!(
            entry.receipt.check_status,
            ReceiptStatus::Skipped | ReceiptStatus::Inconclusive
        )
    }) {
        return VerificationVerdict::Inconclusive;
    }
    let Some(current_snapshot_id) = current_snapshot_id else {
        return VerificationVerdict::Inconclusive;
    };
    let all_applicable = trusted_checks.iter().all(|trusted| {
        receipts.iter().any(|entry| {
            entry.receipt.is_applicable_to(
                &trusted.check_spec,
                current_snapshot_id,
                &policy.verification_scope,
                policy.workspace_trust_requirement,
                WorkspaceTrust::Unknown,
                policy.sandbox_profile,
            )
        })
    });
    if all_applicable {
        VerificationVerdict::Passed
    } else {
        VerificationVerdict::Stale
    }
}
