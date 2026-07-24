use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use sigil_kernel::{
    ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType, FileType,
    IntegrationPlanId, IntegrationProjection, IntegrationPromotionAttemptId,
    IntegrationPromotionEffect, IntegrationPromotionRecorded, IntegrationPromotionStatus,
    IntegrationPromotionTarget, IsolatedWorkspaceCleanupStatus, JsonlSessionStore,
    MutationBatchFinished, MutationBatchStatus, MutationCommitted, MutationPrepared,
    MutationSubject, Session, SessionLogEntry, SessionStreamRecord, VerificationPolicy,
    VerificationScope, build_workspace_snapshot, file_content_hash, stable_workspace_id,
};

use super::{MAX_INTEGRATION_GIT_OUTPUT_BYTES, git_optional_text, run_git_bytes, unix_time_ms};
use crate::isolated_workspace::{GitWorktreeCleanupRequest, cleanup_git_worktree};

/// One promoted attempt recovered without replaying its physical effect or parent checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredIntegrationPromotion {
    pub plan_id: IntegrationPlanId,
    pub attempt_id: IntegrationPromotionAttemptId,
    pub preview_digest: String,
    pub promoted_snapshot_id: String,
    pub retained_owned_workspace_id: Option<String>,
}

/// Bounded startup summary for interrupted integration promotions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrationPromotionReconciliation {
    pub inspected: usize,
    pub reconciled: usize,
    pub promoted: usize,
    pub cancelled: usize,
    pub failed: usize,
    pub needs_review: usize,
    pub cleanup_failed: usize,
    pub recovered_promotions: Vec<RecoveredIntegrationPromotion>,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone)]
struct PendingPromotion {
    prepared: IntegrationPromotionRecorded,
    policy: Option<VerificationPolicy>,
    projection_consistent: bool,
}

#[derive(Debug)]
enum RecoveryDecision {
    Terminal {
        status: IntegrationPromotionStatus,
        effect: Option<IntegrationPromotionEffect>,
        promoted_snapshot_id: Option<String>,
        retain_workspace: bool,
        requires_review: bool,
        reason: String,
    },
    NeedsReview(String),
}

/// Reconciles promotion attempts stranded after their durable `Prepared` fact.
///
/// Recovery is observation-only with respect to the promoted target: it never reapplies a
/// changeset, advances a ref, runs a check, or starts a provider. A terminal fact is appended only
/// when current Git state or a complete RFC-0002 mutation batch uniquely proves the outcome.
///
/// # Errors
///
/// Returns an error when the parent workspace or durable session stream cannot be read, or when a
/// derived terminal fact cannot be appended. Per-attempt ambiguity and owned-worktree cleanup
/// failures are retained in the returned report for explicit review.
pub async fn reconcile_integration_promotions(
    session: &mut Session,
    parent_workspace_root: &Path,
) -> Result<IntegrationPromotionReconciliation> {
    let parent_workspace_root =
        std::fs::canonicalize(parent_workspace_root).with_context(|| {
            format!(
                "failed to resolve parent workspace for promotion reconciliation: {}",
                parent_workspace_root.display()
            )
        })?;
    let projection = IntegrationProjection::from_entries(session.entries());
    let policies = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::VerificationPolicyChanged(entry)) => {
                Some((entry.policy_hash.clone(), entry.policy.clone()))
            }
            _ => None,
        })
        .collect::<BTreeMap<_, _>>();
    let pending = pending_promotions(&projection, &policies);
    let durable_records = match session.store_path() {
        Some(path) => Some(JsonlSessionStore::read_event_records(path)?),
        None => None,
    };
    let mut report = IntegrationPromotionReconciliation::default();

    for pending in pending {
        report.inspected += 1;
        if !pending.projection_consistent {
            record_needs_review(
                &mut report,
                &pending.prepared,
                "integration promotion projection is inconsistent",
            );
            continue;
        }
        let Some(attempt_id) = pending.prepared.attempt_id.clone() else {
            record_needs_review(
                &mut report,
                &pending.prepared,
                "prepared promotion has no attempt identity",
            );
            continue;
        };
        let decision = match &pending.prepared.target {
            IntegrationPromotionTarget::GitRefAdvance {
                target_ref,
                expected_old_oid,
                candidate_oid,
            } => {
                recover_git_ref_promotion(
                    &parent_workspace_root,
                    &pending,
                    target_ref,
                    expected_old_oid,
                    candidate_oid,
                )
                .await
            }
            IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id,
                expected_revision,
            } => {
                recover_workspace_promotion(
                    &parent_workspace_root,
                    &pending,
                    &attempt_id,
                    expected_snapshot_id,
                    *expected_revision,
                    durable_records.as_deref(),
                )
                .await
            }
        };
        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                record_needs_review(&mut report, &pending.prepared, &format!("{error:#}"));
                continue;
            }
        };
        let RecoveryDecision::Terminal {
            status,
            effect,
            promoted_snapshot_id,
            retain_workspace,
            requires_review,
            reason,
        } = decision
        else {
            let RecoveryDecision::NeedsReview(reason) = decision else {
                unreachable!();
            };
            record_needs_review(&mut report, &pending.prepared, &reason);
            continue;
        };

        let mut terminal = pending.prepared.clone();
        terminal.status = status;
        terminal.effect = effect;
        terminal.reason = Some(reason);
        terminal.recorded_at_unix_ms =
            unix_time_ms().max(pending.prepared.recorded_at_unix_ms.saturating_add(1));
        session.append_control(ControlEntry::IntegrationPromotionRecorded(terminal.clone()))?;
        report.reconciled += 1;
        match status {
            IntegrationPromotionStatus::Promoted => report.promoted += 1,
            IntegrationPromotionStatus::Cancelled => report.cancelled += 1,
            IntegrationPromotionStatus::Failed => report.failed += 1,
            _ => {}
        }
        if requires_review {
            report.needs_review += 1;
            report.failures.push(format!(
                "{}: {}",
                attempt_id.as_str(),
                terminal.reason.as_deref().unwrap_or("review required")
            ));
        }
        if let Some(promoted_snapshot_id) = promoted_snapshot_id {
            report
                .recovered_promotions
                .push(RecoveredIntegrationPromotion {
                    plan_id: terminal.plan_id.clone(),
                    attempt_id: attempt_id.clone(),
                    preview_digest: terminal.preview_digest.clone(),
                    promoted_snapshot_id,
                    retained_owned_workspace_id: retain_workspace.then(|| {
                        terminal
                            .recovery_binding
                            .as_ref()
                            .expect("recovered terminal retains prepared binding")
                            .owned_workspace_id
                            .clone()
                    }),
                });
        }
        if !retain_workspace {
            cleanup_recovered_workspace(
                &parent_workspace_root,
                &terminal,
                &mut report,
                &attempt_id,
            )
            .await;
        }
    }
    Ok(report)
}

fn pending_promotions(
    projection: &IntegrationProjection,
    policies: &BTreeMap<String, VerificationPolicy>,
) -> Vec<PendingPromotion> {
    let mut pending = Vec::new();
    for state in projection.plans.values() {
        for prepared in state
            .promotions
            .iter()
            .filter(|record| record.status == IntegrationPromotionStatus::Prepared)
        {
            let terminal_exists = prepared.attempt_id.as_ref().is_some_and(|attempt_id| {
                state.promotions.iter().any(|record| {
                    record.attempt_id.as_ref() == Some(attempt_id)
                        && record.status != IntegrationPromotionStatus::Prepared
                })
            });
            if terminal_exists {
                continue;
            }
            let policy = state
                .promotion_previews
                .get(&prepared.preview_digest)
                .and_then(|preview| policies.get(preview.policy_digest.as_str()))
                .cloned();
            pending.push(PendingPromotion {
                prepared: prepared.clone(),
                policy,
                projection_consistent: !state.inconsistent,
            });
        }
    }
    pending
}

async fn recover_git_ref_promotion(
    parent_workspace_root: &Path,
    pending: &PendingPromotion,
    target_ref: &str,
    expected_old_oid: &str,
    candidate_oid: &str,
) -> Result<RecoveryDecision> {
    let observed = git_optional_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(target_ref),
        ],
    )
    .await?;
    if observed.as_deref() == Some(expected_old_oid) {
        return Ok(RecoveryDecision::Terminal {
            status: IntegrationPromotionStatus::Cancelled,
            effect: None,
            promoted_snapshot_id: None,
            retain_workspace: false,
            requires_review: false,
            reason: "recovery observed the expected old ref; no promotion effect is present"
                .to_owned(),
        });
    }
    if observed.as_deref() != Some(candidate_oid) {
        return Ok(RecoveryDecision::NeedsReview(format!(
            "promotion ref is neither the expected old object nor the exact candidate (observed {})",
            observed.as_deref().unwrap_or("missing")
        )));
    }
    let binding = pending
        .prepared
        .recovery_binding
        .as_ref()
        .ok_or_else(|| anyhow!("prepared Git promotion has no recovery binding"))?;
    let retained_candidate_is_exact = match pending.policy.as_ref() {
        Some(policy) => verify_retained_candidate(
            parent_workspace_root,
            &binding.owned_workspace_id,
            &binding.candidate_snapshot_id,
            policy,
        )
        .await
        .unwrap_or(false),
        None => false,
    };
    Ok(RecoveryDecision::Terminal {
        status: IntegrationPromotionStatus::Promoted,
        effect: Some(IntegrationPromotionEffect::GitRefAdvanced {
            old_oid: expected_old_oid.to_owned(),
            new_oid: candidate_oid.to_owned(),
        }),
        promoted_snapshot_id: Some(binding.candidate_snapshot_id.clone()),
        retain_workspace: true,
        requires_review: !retained_candidate_is_exact,
        reason: if retained_candidate_is_exact {
            "recovery observed the exact candidate ref and retained authoritative checkout"
                .to_owned()
        } else {
            "recovery observed the exact candidate ref, but its authoritative checkout requires review"
                .to_owned()
        },
    })
}

async fn recover_workspace_promotion(
    parent_workspace_root: &Path,
    pending: &PendingPromotion,
    attempt_id: &IntegrationPromotionAttemptId,
    expected_snapshot_id: &str,
    expected_revision: u64,
    durable_records: Option<&[SessionStreamRecord]>,
) -> Result<RecoveryDecision> {
    let Some(durable_records) = durable_records else {
        return Ok(RecoveryDecision::NeedsReview(
            "workspace promotion recovery requires a durable mutation stream".to_owned(),
        ));
    };
    let evidence = WorkspaceMutationEvidence::from_records(
        durable_records,
        &format!("promotion-{}", attempt_id.as_str()),
    )?;
    if evidence.prepared.is_empty() {
        let current = capture_snapshot(
            parent_workspace_root,
            &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        )
        .await?;
        return Ok(if current == expected_snapshot_id {
            RecoveryDecision::Terminal {
                status: IntegrationPromotionStatus::Cancelled,
                effect: None,
                promoted_snapshot_id: None,
                retain_workspace: false,
                requires_review: false,
                reason: "recovery found no mutation evidence and the parent snapshot is unchanged"
                    .to_owned(),
            }
        } else {
            RecoveryDecision::NeedsReview(
                "parent snapshot changed without attempt-bound mutation evidence".to_owned(),
            )
        });
    }
    let batch_id = evidence.exact_batch_id()?;
    let terminal = evidence.exact_terminal(batch_id)?;
    evidence.validate_terminal_bindings(batch_id, terminal)?;

    match terminal.status {
        MutationBatchStatus::Applied => {
            evidence.validate_complete_apply(batch_id, terminal)?;
            evidence.validate_current_apply_state(parent_workspace_root, batch_id)?;
            let policy = pending.policy.as_ref().ok_or_else(|| {
                anyhow!("promotion preview policy is unavailable for authoritative snapshot")
            })?;
            let promoted_snapshot_id =
                capture_snapshot(parent_workspace_root, &policy.verification_scope).await?;
            let expected_parent_snapshot_id = pending
                .prepared
                .recovery_binding
                .as_ref()
                .and_then(|binding| binding.expected_parent_snapshot_id.as_deref())
                .ok_or_else(|| {
                    anyhow!("prepared promotion has no expected parent snapshot binding")
                })?;
            if promoted_snapshot_id != expected_parent_snapshot_id {
                return Ok(RecoveryDecision::NeedsReview(
                    "current parent snapshot does not match the prepared aggregate candidate"
                        .to_owned(),
                ));
            }
            Ok(RecoveryDecision::Terminal {
                status: IntegrationPromotionStatus::Promoted,
                effect: Some(IntegrationPromotionEffect::WorkspaceApplied {
                    promoted_snapshot_id: promoted_snapshot_id.clone(),
                    promoted_revision: expected_revision.saturating_add(1),
                }),
                promoted_snapshot_id: Some(promoted_snapshot_id),
                retain_workspace: false,
                requires_review: false,
                reason: "recovery proved a complete attempt-bound workspace mutation batch"
                    .to_owned(),
            })
        }
        MutationBatchStatus::Failed | MutationBatchStatus::RolledBack => {
            Ok(RecoveryDecision::Terminal {
                status: IntegrationPromotionStatus::Failed,
                effect: None,
                promoted_snapshot_id: None,
                retain_workspace: false,
                requires_review: false,
                reason: format!(
                    "recovery observed terminal workspace mutation status {:?}",
                    terminal.status
                ),
            })
        }
        MutationBatchStatus::PartiallyApplied | MutationBatchStatus::RollbackFailed => {
            Ok(RecoveryDecision::Terminal {
                status: IntegrationPromotionStatus::Failed,
                effect: None,
                promoted_snapshot_id: None,
                retain_workspace: false,
                requires_review: true,
                reason: format!(
                    "workspace mutation ended {:?}; residual parent changes require review",
                    terminal.status
                ),
            })
        }
    }
}

#[derive(Debug, Default)]
struct WorkspaceMutationEvidence {
    prepared: Vec<MutationPrepared>,
    committed: Vec<MutationCommitted>,
    terminals: Vec<MutationBatchFinished>,
}

impl WorkspaceMutationEvidence {
    fn from_records(records: &[SessionStreamRecord], tool_call_id: &str) -> Result<Self> {
        let mut evidence = Self::default();
        for record in records {
            let event = record.stored_event();
            match event.event_kind() {
                Some(DurableEventType::MutationPrepared) => {
                    let prepared =
                        serde_json::from_value::<MutationPrepared>(event.payload.clone())
                            .context("failed to decode promotion mutation prepare")?;
                    if prepared.tool_call_id.as_deref() == Some(tool_call_id) {
                        evidence.prepared.push(prepared);
                    }
                }
                Some(DurableEventType::MutationCommitted) => {
                    evidence.committed.push(
                        serde_json::from_value::<MutationCommitted>(event.payload.clone())
                            .context("failed to decode promotion mutation commit")?,
                    );
                }
                Some(DurableEventType::MutationBatchFinished) => {
                    evidence.terminals.push(
                        serde_json::from_value::<MutationBatchFinished>(event.payload.clone())
                            .context("failed to decode promotion mutation batch terminal")?,
                    );
                }
                _ => {}
            }
        }
        Ok(evidence)
    }

    fn exact_batch_id(&self) -> Result<&str> {
        let batch_ids = self
            .prepared
            .iter()
            .map(|prepared| {
                prepared
                    .batch_id
                    .as_deref()
                    .ok_or_else(|| anyhow!("promotion mutation prepare has no batch identity"))
            })
            .collect::<Result<BTreeSet<_>>>()?;
        if batch_ids.len() != 1 {
            bail!("promotion mutation evidence spans multiple batches");
        }
        Ok(batch_ids
            .into_iter()
            .next()
            .expect("one promotion mutation batch"))
    }

    fn exact_terminal(&self, batch_id: &str) -> Result<&MutationBatchFinished> {
        let terminals = self
            .terminals
            .iter()
            .filter(|terminal| terminal.batch_id == batch_id)
            .collect::<Vec<_>>();
        if terminals.len() != 1 {
            bail!(
                "promotion mutation batch has {} terminal records",
                terminals.len()
            );
        }
        Ok(terminals[0])
    }

    fn validate_terminal_bindings(
        &self,
        batch_id: &str,
        terminal: &MutationBatchFinished,
    ) -> Result<()> {
        let prepared_ids = self
            .prepared
            .iter()
            .map(|prepared| prepared.operation_id.as_str())
            .collect::<BTreeSet<_>>();
        let committed = self
            .committed
            .iter()
            .filter(|committed| committed.batch_id.as_deref() == Some(batch_id))
            .collect::<Vec<_>>();
        let committed_ids = committed
            .iter()
            .map(|committed| committed.operation_id.as_str())
            .collect::<BTreeSet<_>>();
        let terminal_committed = terminal
            .committed_operations
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if committed.len() != committed_ids.len() || terminal_committed != committed_ids {
            bail!("promotion mutation commit records do not match the batch terminal");
        }
        if !committed_ids.is_subset(&prepared_ids) {
            bail!("promotion mutation commit has no attempt-bound prepare");
        }
        for committed in committed {
            let prepared = self
                .prepared
                .iter()
                .find(|prepared| prepared.operation_id == committed.operation_id)
                .expect("validated prepared operation");
            if prepared.batch_id != committed.batch_id
                || prepared.workspace_id != committed.workspace_id.clone().unwrap_or_default()
                || prepared.subject != committed.committed_subject
                || prepared.intended_after_hash != committed.observed_after_hash
            {
                bail!("promotion mutation prepare/commit binding is inconsistent");
            }
        }
        Ok(())
    }

    fn validate_complete_apply(
        &self,
        batch_id: &str,
        terminal: &MutationBatchFinished,
    ) -> Result<()> {
        let prepared_ids = self
            .prepared
            .iter()
            .map(|prepared| prepared.operation_id.as_str())
            .collect::<BTreeSet<_>>();
        let committed_ids = terminal
            .committed_operations
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if prepared_ids.len() != self.prepared.len()
            || committed_ids != prepared_ids
            || !terminal.failed_operations.is_empty()
            || !terminal.rollback_operations.is_empty()
            || !terminal.rollback_failed_operations.is_empty()
            || self
                .committed
                .iter()
                .filter(|committed| committed.batch_id.as_deref() == Some(batch_id))
                .count()
                != self.prepared.len()
        {
            bail!("applied promotion mutation batch is incomplete or ambiguous");
        }
        Ok(())
    }

    fn validate_current_apply_state(&self, workspace_root: &Path, batch_id: &str) -> Result<()> {
        let workspace_id = stable_workspace_id(workspace_root)?;
        if self
            .prepared
            .iter()
            .any(|prepared| prepared.workspace_id != workspace_id)
        {
            bail!("promotion mutation evidence belongs to another workspace");
        }
        for committed in self
            .committed
            .iter()
            .filter(|committed| committed.batch_id.as_deref() == Some(batch_id))
        {
            let MutationSubject::File {
                path,
                file_type: FileType::File,
            } = &committed.committed_subject
            else {
                bail!("promotion mutation commit has an unsupported subject");
            };
            if path.is_absolute()
                || path
                    .components()
                    .any(|component| !matches!(component, Component::Normal(_)))
            {
                bail!("promotion mutation commit path is unsafe");
            }
            let absolute_path = workspace_root.join(path);
            match committed.observed_after_hash.as_deref() {
                Some(expected_hash) => {
                    let metadata =
                        std::fs::symlink_metadata(&absolute_path).with_context(|| {
                            format!(
                                "failed to inspect promoted mutation subject {}",
                                path.display()
                            )
                        })?;
                    if !metadata.file_type().is_file()
                        || metadata.file_type().is_symlink()
                        || file_content_hash(&absolute_path)?.as_deref() != Some(expected_hash)
                    {
                        bail!("promoted mutation subject changed after its durable commit");
                    }
                }
                None => match std::fs::symlink_metadata(&absolute_path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Ok(_) => bail!("deleted promotion mutation subject exists during recovery"),
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!(
                                "failed to inspect deleted mutation subject {}",
                                path.display()
                            )
                        });
                    }
                },
            }
        }
        Ok(())
    }
}

async fn verify_retained_candidate(
    parent_workspace_root: &Path,
    owned_workspace_id: &str,
    expected_snapshot_id: &str,
    policy: &VerificationPolicy,
) -> Result<bool> {
    let inventory = run_git_bytes(
        parent_workspace_root,
        [
            OsString::from("worktree"),
            OsString::from("list"),
            OsString::from("--porcelain"),
        ],
        MAX_INTEGRATION_GIT_OUTPUT_BYTES,
    )
    .await?;
    let inventory =
        String::from_utf8(inventory).context("Git worktree inventory is not valid UTF-8")?;
    let workspace_root = inventory.lines().find_map(|line| {
        let path = line.strip_prefix("worktree ")?;
        let path = PathBuf::from(path);
        (path.file_name().and_then(|name| name.to_str()) == Some(owned_workspace_id))
            .then_some(path)
    });
    let Some(workspace_root) = workspace_root else {
        return Ok(false);
    };
    Ok(
        capture_snapshot(&workspace_root, &policy.verification_scope).await?
            == expected_snapshot_id,
    )
}

async fn capture_snapshot(root: &Path, scope: &VerificationScope) -> Result<String> {
    let root = root.to_path_buf();
    let scope = scope.clone();
    tokio::task::spawn_blocking(move || {
        let workspace_id = stable_workspace_id(&root)?;
        build_workspace_snapshot(&root, workspace_id, &scope, 0)?
            .workspace_snapshot_id
            .ok_or_else(|| anyhow!("promotion recovery snapshot is incomplete"))
    })
    .await
    .context("promotion recovery snapshot task failed")?
}

async fn cleanup_recovered_workspace(
    parent_workspace_root: &Path,
    terminal: &IntegrationPromotionRecorded,
    report: &mut IntegrationPromotionReconciliation,
    attempt_id: &IntegrationPromotionAttemptId,
) {
    let Some(binding) = terminal.recovery_binding.as_ref() else {
        report.cleanup_failed += 1;
        report.failures.push(format!(
            "{}: recovered promotion has no owned workspace binding",
            attempt_id.as_str()
        ));
        return;
    };
    match cleanup_git_worktree(GitWorktreeCleanupRequest {
        parent_workspace_root: parent_workspace_root.to_path_buf(),
        isolated_workspace_id: binding.owned_workspace_id.clone(),
    })
    .await
    {
        Ok(receipt)
            if matches!(
                receipt.status,
                IsolatedWorkspaceCleanupStatus::Removed
                    | IsolatedWorkspaceCleanupStatus::AlreadyMissing
            ) => {}
        Ok(receipt) => {
            report.cleanup_failed += 1;
            report.failures.push(format!(
                "{}: unexpected promotion workspace cleanup status {}",
                attempt_id.as_str(),
                receipt.status.as_str()
            ));
        }
        Err(error) => {
            report.cleanup_failed += 1;
            report.failures.push(format!(
                "{}: promotion workspace cleanup failed: {error:#}",
                attempt_id.as_str()
            ));
        }
    }
}

fn record_needs_review(
    report: &mut IntegrationPromotionReconciliation,
    prepared: &IntegrationPromotionRecorded,
    reason: &str,
) {
    report.needs_review += 1;
    report.failures.push(format!(
        "{}: {reason}",
        prepared
            .attempt_id
            .as_ref()
            .map_or("unknown-attempt", IntegrationPromotionAttemptId::as_str)
    ));
}
