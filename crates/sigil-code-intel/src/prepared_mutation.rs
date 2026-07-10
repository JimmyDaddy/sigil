use std::{fs, path::PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use sigil_kernel::{
    FileType, MutationBatchStatus, MutationCoordinator, MutationEventRecorder, MutationReconciled,
    MutationResolution, MutationSubject, OperationId, PreparedFileMutation,
    PreparedToolAuditBinding, SnapshotCoverage, ToolPreview, ToolPreviewFile, bytes_hash,
    file_content_hash, stable_event_hash,
};

use crate::{
    edit::{apply_text_edits, render_unified_diff},
    service::CodeEditPlan,
    workspace::resolve_workspace_file,
};

#[derive(Debug)]
struct PreparedMutationFile {
    path: String,
    absolute_path: PathBuf,
    before_content: String,
    proposed_content: String,
    before_hash: String,
    after_hash: String,
    diff: String,
    edit_count: usize,
}

/// Code-intel-owned immutable LSP mutation artifact.
///
/// Raw source and proposed bytes are process-local and intentionally not serializable or cloneable.
#[derive(Debug)]
pub(crate) struct PreparedMutation {
    plan: CodeEditPlan,
    source_absolute_path: PathBuf,
    files: Vec<PreparedMutationFile>,
    base_workspace_revision: u64,
    base_workspace_epoch: u64,
    content_digest: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedMutationStatus {
    Applied,
    RolledBack,
    RollbackFailed,
    Failed,
    Stale,
}

#[derive(Debug)]
pub(crate) struct PreparedMutationOutcome {
    pub status: PreparedMutationStatus,
    pub server: String,
    pub capability: String,
    pub query_elapsed_ms: u64,
    pub applied_edits: usize,
    pub changed_files: Vec<String>,
    pub residual_files: Vec<String>,
    pub committed_operations: Vec<OperationId>,
    pub rollback_operations: Vec<OperationId>,
    pub failed_operations: Vec<OperationId>,
    pub rollback_failed_operations: Vec<OperationId>,
    pub batch_id: Option<String>,
    pub base_workspace_revision: u64,
    pub reason: Option<String>,
}

impl PreparedMutation {
    pub(crate) fn materialize(
        workspace_root: &std::path::Path,
        recorder: Option<&MutationEventRecorder>,
        plan: CodeEditPlan,
    ) -> Result<Self> {
        if plan.edit.external_changes_filtered > 0 || plan.edit.unsupported_changes_filtered > 0 {
            bail!(
                "prepared mutation rejected an incomplete LSP edit set: external_filtered={} unsupported_filtered={}",
                plan.edit.external_changes_filtered,
                plan.edit.unsupported_changes_filtered
            );
        }

        let base_workspace_revision = recorder
            .map(|recorder| recorder.current_workspace_revision(workspace_root))
            .transpose()?
            .unwrap_or_default();
        let base_workspace_epoch = recorder
            .map(|recorder| recorder.current_workspace_mutation_epoch(workspace_root))
            .transpose()?
            .unwrap_or_default();
        let canonical_workspace_root = fs::canonicalize(workspace_root)
            .with_context(|| format!("failed to resolve {}", workspace_root.display()))?;
        let source_absolute_path = resolve_workspace_file(workspace_root, &plan.source_path)?;
        if canonical_workspace_root.join(&plan.source_path) != source_absolute_path {
            bail!("prepared mutation source must not traverse a symlink");
        }
        let observed_source_hash = file_content_hash(&source_absolute_path)?
            .ok_or_else(|| anyhow!("LSP source disappeared before mutation preparation"))?;
        if observed_source_hash != plan.source_hash {
            bail!("LSP source changed before mutation preparation");
        }

        let mut files = Vec::with_capacity(plan.edit.files.len());
        for file in &plan.edit.files {
            let absolute_path = resolve_workspace_file(workspace_root, &file.path)?;
            if canonical_workspace_root.join(&file.path) != absolute_path {
                bail!(
                    "prepared mutation target must not traverse a symlink: {}",
                    file.path
                );
            }
            let before_content = fs::read_to_string(&absolute_path)
                .with_context(|| format!("failed to read {}", absolute_path.display()))?;
            let proposed_content = apply_text_edits(&before_content, &file.edits)
                .with_context(|| format!("failed to prepare edits for {}", file.path))?;
            let before_hash = bytes_hash(before_content.as_bytes());
            let after_hash = bytes_hash(proposed_content.as_bytes());
            let diff = render_unified_diff(
                &before_content,
                &proposed_content,
                &format!("current/{}", file.path),
                &format!("proposed/{}", file.path),
            );
            files.push(PreparedMutationFile {
                path: file.path.clone(),
                absolute_path,
                before_content,
                proposed_content,
                before_hash,
                after_hash,
                diff,
                edit_count: file.edits.len(),
            });
        }

        let terminal_workspace_revision = recorder
            .map(|recorder| recorder.current_workspace_revision(workspace_root))
            .transpose()?
            .unwrap_or_default();
        let terminal_workspace_epoch = recorder
            .map(|recorder| recorder.current_workspace_mutation_epoch(workspace_root))
            .transpose()?
            .unwrap_or_default();
        if terminal_workspace_revision != base_workspace_revision {
            bail!("workspace revision changed while preparing LSP mutation");
        }
        if terminal_workspace_epoch != base_workspace_epoch {
            bail!("workspace mutation epoch changed while preparing LSP mutation");
        }

        let digest_material = json!({
            "schema_version": 1,
            "source": {
                "kind": "workspace_local_lsp",
                "server": plan.server,
                "capability": plan.capability,
                "path": plan.source_path,
                "document_version": plan.source_version,
                "content_hash": plan.source_hash,
            },
            "base_workspace_revision": base_workspace_revision,
            "base_workspace_epoch": base_workspace_epoch,
            "edit_set": plan.edit,
            "files": files.iter().map(|file| json!({
                "path": file.path,
                "before_hash": file.before_hash,
                "after_hash": file.after_hash,
                "preview_hash": stable_event_hash(file.diff.as_bytes()),
            })).collect::<Vec<_>>(),
        });
        let content_digest = stable_event_hash(
            serde_json::to_vec(&digest_material)
                .context("failed to encode prepared mutation digest material")?,
        );

        Ok(Self {
            plan,
            source_absolute_path,
            files,
            base_workspace_revision,
            base_workspace_epoch,
            content_digest,
        })
    }

    pub(crate) fn content_digest(&self) -> &str {
        &self.content_digest
    }

    pub(crate) fn target_paths(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|file| file.path.as_str())
    }

    pub(crate) fn preview(&self, title: &str) -> ToolPreview {
        let file_diffs = self
            .files
            .iter()
            .map(|file| ToolPreviewFile {
                path: file.path.clone(),
                diff: file.diff.clone(),
            })
            .collect::<Vec<_>>();
        ToolPreview {
            title: format!("{title} ({})", self.plan.server),
            summary: format!(
                "{} edits across {} file(s) via {}",
                self.plan.edit.total_edits(),
                self.files.len(),
                self.plan.capability
            ),
            body: file_diffs
                .iter()
                .map(|file| file.diff.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            changed_files: self.files.iter().map(|file| file.path.clone()).collect(),
            file_diffs,
        }
    }

    pub(crate) fn execute(
        self,
        workspace_root: &std::path::Path,
        recorder: &MutationEventRecorder,
        binding: &PreparedToolAuditBinding,
        tool_call_id: &str,
        cancellation: Option<&sigil_kernel::RunCancellationHandle>,
    ) -> Result<PreparedMutationOutcome> {
        if binding.content_digest != self.content_digest {
            return Ok(self.stale("prepared_digest_mismatch"));
        }
        let batch_id = format!("prepared:{}", binding.prepared_digest);
        let coordinator = recorder.coordinator_with_workspace_lease(
            workspace_root,
            tool_call_id.to_owned(),
            Some(batch_id.clone()),
        )?;
        if coordinator.workspace_mutation_epoch()? != self.base_workspace_epoch {
            return Ok(self.stale("workspace_mutation_epoch_changed"));
        }
        if recorder.current_workspace_revision(workspace_root)? != self.base_workspace_revision {
            return Ok(self.stale("workspace_revision_changed"));
        }
        let observed_source_path = resolve_workspace_file(workspace_root, &self.plan.source_path);
        if observed_source_path.as_ref().ok() != Some(&self.source_absolute_path) {
            return Ok(self.stale("source_identity_changed"));
        }
        if file_content_hash(&self.source_absolute_path)?.as_deref()
            != Some(self.plan.source_hash.as_str())
        {
            return Ok(self.stale("source_changed"));
        }
        let mut stale_target = None;
        for file in &self.files {
            if resolve_workspace_file(workspace_root, &file.path)
                .as_ref()
                .ok()
                != Some(&file.absolute_path)
            {
                stale_target = Some(format!("target_identity_changed:{}", file.path));
                break;
            }
            if file_content_hash(&file.absolute_path)?.as_deref() != Some(file.before_hash.as_str())
            {
                stale_target = Some(format!("target_changed:{}", file.path));
                break;
            }
        }
        if let Some(reason) = stale_target {
            return Ok(self.stale(&reason));
        }

        let expected_subjects = self
            .files
            .iter()
            .map(|file| MutationSubject::File {
                path: PathBuf::from(&file.path),
                file_type: FileType::File,
            })
            .collect::<Vec<_>>();
        recorder.append_bound_batch_started(
            &batch_id,
            &format!("prepared_mutation:{tool_call_id}"),
            &expected_subjects,
            Some(&binding.prepared_digest),
            Some(&binding.approval_identity),
            Some(&binding.policy_fingerprint),
        )?;
        let mut prepared_files =
            Vec::<Option<PreparedFileMutation>>::with_capacity(self.files.len());
        for file in &self.files {
            if file.before_hash == file.after_hash {
                prepared_files.push(None);
                continue;
            }
            match coordinator.prepare_file_expected(
                PathBuf::from(&file.path),
                file.absolute_path.clone(),
                Some(file.before_hash.clone()),
                Some(file.after_hash.clone()),
            ) {
                Ok(prepared)
                    if matches!(prepared.snapshot_coverage, SnapshotCoverage::Captured(_)) =>
                {
                    prepared_files.push(Some(prepared));
                }
                Ok(prepared) => {
                    let failed = vec![prepared.operation_id.clone()];
                    coordinator.reconcile_prepared_file_from_disk(&prepared)?;
                    reconcile_uncommitted(&coordinator, &prepared_files, 0)?;
                    recorder.append_bound_batch_finished(
                        &batch_id,
                        MutationBatchStatus::Failed,
                        &[],
                        &failed,
                        &[],
                        &[],
                        Some(&binding.prepared_digest),
                        Some(&binding.approval_identity),
                        Some(&binding.policy_fingerprint),
                    )?;
                    return Ok(self.failed(
                        batch_id,
                        failed,
                        "rollback snapshot coverage unavailable before apply",
                    ));
                }
                Err(error) => {
                    reconcile_uncommitted(&coordinator, &prepared_files, 0)?;
                    let failed = Vec::new();
                    recorder.append_bound_batch_finished(
                        &batch_id,
                        MutationBatchStatus::Failed,
                        &[],
                        &failed,
                        &[],
                        &[],
                        Some(&binding.prepared_digest),
                        Some(&binding.approval_identity),
                        Some(&binding.policy_fingerprint),
                    )?;
                    return Ok(self.failed(batch_id, failed, &error.to_string()));
                }
            }
        }

        let mut committed_operations = Vec::new();
        let mut failed_operations = Vec::new();
        let mut failed_index = None;
        for (index, prepared) in prepared_files.iter().enumerate() {
            let Some(prepared) = prepared else {
                continue;
            };
            let forward_effect = cancellation
                .map(|handle| {
                    handle.begin_effect(
                        sigil_kernel::RunEffectClass::Forward,
                        sigil_kernel::RunEffectKind::Tool,
                    )
                })
                .transpose();
            let _forward_effect = match forward_effect {
                Ok(effect) => effect,
                Err(_) => {
                    failed_operations.push(prepared.operation_id.clone());
                    failed_index = Some(index);
                    break;
                }
            };
            match coordinator.commit_write(prepared, self.files[index].proposed_content.as_bytes())
            {
                Ok(committed) => committed_operations.push(committed.operation_id),
                Err(_error) => {
                    failed_operations.push(prepared.operation_id.clone());
                    failed_index = Some(index);
                    break;
                }
            }
        }

        let Some(failed_index) = failed_index else {
            recorder.append_bound_batch_finished(
                &batch_id,
                MutationBatchStatus::Applied,
                &committed_operations,
                &[],
                &[],
                &[],
                Some(&binding.prepared_digest),
                Some(&binding.approval_identity),
                Some(&binding.policy_fingerprint),
            )?;
            return Ok(PreparedMutationOutcome {
                status: PreparedMutationStatus::Applied,
                server: self.plan.server,
                capability: self.plan.capability,
                query_elapsed_ms: self.plan.metadata.elapsed_ms,
                applied_edits: self.files.iter().map(|file| file.edit_count).sum(),
                changed_files: self
                    .files
                    .iter()
                    .filter(|file| file.before_hash != file.after_hash)
                    .map(|file| file.path.clone())
                    .collect(),
                residual_files: Vec::new(),
                committed_operations,
                rollback_operations: Vec::new(),
                failed_operations: Vec::new(),
                rollback_failed_operations: Vec::new(),
                batch_id: Some(batch_id),
                base_workspace_revision: self.base_workspace_revision,
                reason: None,
            });
        };

        reconcile_uncommitted(
            &coordinator,
            &prepared_files,
            failed_index.saturating_add(1),
        )?;

        let mut rollback_operations = Vec::new();
        let mut rollback_failed_operations = Vec::new();
        let mut residual_files = Vec::new();
        for index in (0..=failed_index).rev() {
            let _cleanup_effect = cancellation
                .map(|handle| {
                    handle.begin_effect(
                        sigil_kernel::RunEffectClass::Cleanup,
                        sigil_kernel::RunEffectKind::Tool,
                    )
                })
                .transpose()?;
            let file = &self.files[index];
            let Some(original_prepared) = prepared_files[index].as_ref() else {
                continue;
            };
            let observed = match file_content_hash(&file.absolute_path) {
                Ok(observed) => observed,
                Err(_) => {
                    if index == failed_index {
                        coordinator.reconcile_prepared_file_from_disk(original_prepared)?;
                    }
                    rollback_failed_operations.push(original_prepared.operation_id.clone());
                    residual_files.push(file.path.clone());
                    continue;
                }
            };
            if observed.as_deref() == Some(file.before_hash.as_str()) {
                if index == failed_index {
                    coordinator.reconcile_prepared_file_from_disk(original_prepared)?;
                }
                continue;
            }
            if observed.as_deref() != Some(file.after_hash.as_str()) {
                if index == failed_index {
                    coordinator.reconcile_prepared_file_from_disk(original_prepared)?;
                }
                rollback_failed_operations.push(original_prepared.operation_id.clone());
                residual_files.push(file.path.clone());
                continue;
            }
            let rollback_prepared = match coordinator.prepare_file_expected(
                PathBuf::from(&file.path),
                file.absolute_path.clone(),
                Some(file.after_hash.clone()),
                Some(file.before_hash.clone()),
            ) {
                Ok(prepared) => prepared,
                Err(_) => {
                    if index == failed_index {
                        coordinator.reconcile_prepared_file_from_disk(original_prepared)?;
                    }
                    rollback_failed_operations.push(original_prepared.operation_id.clone());
                    residual_files.push(file.path.clone());
                    continue;
                }
            };
            match coordinator.commit_write(&rollback_prepared, file.before_content.as_bytes()) {
                Ok(committed) => {
                    rollback_operations.push(committed.operation_id);
                    if index == failed_index {
                        coordinator.reconcile_prepared_file_from_disk(original_prepared)?;
                    }
                }
                Err(_) => {
                    let rollback_event =
                        coordinator.reconcile_prepared_file_from_disk(&rollback_prepared)?;
                    let rollback_reconcile =
                        serde_json::from_value::<MutationReconciled>(rollback_event.payload)
                            .context("failed to decode live rollback reconciliation")?;
                    if index == failed_index {
                        coordinator.reconcile_prepared_file_from_disk(original_prepared)?;
                    }
                    if rollback_reconcile.resolution == MutationResolution::MarkCommitted {
                        rollback_operations.push(rollback_prepared.operation_id.clone());
                    } else {
                        rollback_failed_operations.push(rollback_prepared.operation_id.clone());
                        residual_files.push(file.path.clone());
                    }
                }
            }
        }
        let (status, batch_status) = if rollback_failed_operations.is_empty() {
            (
                PreparedMutationStatus::RolledBack,
                MutationBatchStatus::RolledBack,
            )
        } else {
            (
                PreparedMutationStatus::RollbackFailed,
                MutationBatchStatus::RollbackFailed,
            )
        };
        if matches!(status, PreparedMutationStatus::RollbackFailed)
            && let Some(cancellation) = cancellation
        {
            cancellation.mark_cleanup_incomplete();
        }
        recorder.append_bound_batch_finished(
            &batch_id,
            batch_status,
            &committed_operations,
            &failed_operations,
            &rollback_operations,
            &rollback_failed_operations,
            Some(&binding.prepared_digest),
            Some(&binding.approval_identity),
            Some(&binding.policy_fingerprint),
        )?;
        Ok(PreparedMutationOutcome {
            status,
            server: self.plan.server,
            capability: self.plan.capability,
            query_elapsed_ms: self.plan.metadata.elapsed_ms,
            applied_edits: 0,
            changed_files: residual_files.clone(),
            residual_files,
            committed_operations,
            rollback_operations,
            failed_operations,
            rollback_failed_operations,
            batch_id: Some(batch_id),
            base_workspace_revision: self.base_workspace_revision,
            reason: Some("prepared mutation apply failed".to_owned()),
        })
    }

    fn stale(self, reason: &str) -> PreparedMutationOutcome {
        PreparedMutationOutcome {
            status: PreparedMutationStatus::Stale,
            server: self.plan.server,
            capability: self.plan.capability,
            query_elapsed_ms: self.plan.metadata.elapsed_ms,
            applied_edits: 0,
            changed_files: Vec::new(),
            residual_files: Vec::new(),
            committed_operations: Vec::new(),
            rollback_operations: Vec::new(),
            failed_operations: Vec::new(),
            rollback_failed_operations: Vec::new(),
            batch_id: None,
            base_workspace_revision: self.base_workspace_revision,
            reason: Some(reason.to_owned()),
        }
    }

    fn failed(
        self,
        batch_id: String,
        failed_operations: Vec<OperationId>,
        reason: &str,
    ) -> PreparedMutationOutcome {
        PreparedMutationOutcome {
            status: PreparedMutationStatus::Failed,
            server: self.plan.server,
            capability: self.plan.capability,
            query_elapsed_ms: self.plan.metadata.elapsed_ms,
            applied_edits: 0,
            changed_files: Vec::new(),
            residual_files: Vec::new(),
            committed_operations: Vec::new(),
            rollback_operations: Vec::new(),
            failed_operations,
            rollback_failed_operations: Vec::new(),
            batch_id: Some(batch_id),
            base_workspace_revision: self.base_workspace_revision,
            reason: Some(reason.to_owned()),
        }
    }
}

fn reconcile_uncommitted(
    coordinator: &MutationCoordinator,
    prepared_files: &[Option<PreparedFileMutation>],
    start: usize,
) -> Result<()> {
    for prepared in prepared_files.iter().skip(start).flatten() {
        coordinator.reconcile_prepared_file_from_disk(prepared)?;
    }
    Ok(())
}
