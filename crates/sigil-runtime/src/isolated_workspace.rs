//! Physical isolated-workspace materialization owned by the runtime.
//!
//! This module deliberately does not append session events or start child agents. Callers must
//! persist ownership before exposing a materialized workspace to a child and must record cleanup
//! outcomes through the durable write-isolation protocol.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::{ExitStatus, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetId, ChangeSetRisk, ChangeSetValidation,
    ChangeSetValidationKind, ChangeSetValidationStatus, ControlEntry,
    DEFAULT_TASK_VERIFICATION_SCOPE_HASH, IntegrationBaseRepresentation, IntegrationContentClass,
    IntegrationEffect, IntegrationObservedEffect, IntegrationProposalFacts,
    IsolatedWorkspaceBackend, IsolatedWorkspaceCleanupRecorded, IsolatedWorkspaceCleanupStatus,
    MutationArtifactId, MutationEventRecorder, Session, TaskChildChangeSetArtifact,
    TaskChildChangeSetProposal, VerificationScope, WorkspaceSnapshotBuild, WriteIsolationMode,
    build_workspace_snapshot, is_sensitive_mutation_artifact_path, stable_workspace_id,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};

const ISOLATED_WORKTREE_ROOT: &str = "sigil-isolated-worktrees";
const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_OUTPUT_LIMIT: usize = 64 * 1024;
const GIT_ERROR_OUTPUT_LIMIT: usize = 8 * 1024;
const MAX_ISOLATED_WORKSPACE_ID_BYTES: usize = 128;
const MAX_CHANGESET_FILES: usize = 256;
const MAX_CHANGESET_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CHANGESET_ARTIFACT_BYTES: usize = 4 * 1024 * 1024;
const MAX_CHANGESET_PATH_BYTES: usize = 256 * 1024;
const MAX_OVERLAY_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const OVERLAY_MANIFEST_VERSION: u32 = 1;

/// Request for one detached Git worktree bound to an existing parent snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeMaterializationRequest {
    pub parent_workspace_root: PathBuf,
    pub isolated_workspace_id: String,
    pub base_snapshot_id: String,
}

/// Request to freeze one exact Git commit plus safe dirty/untracked overlay.
#[derive(Debug, Clone)]
pub struct GitWorktreeBaseFreezeRequest {
    pub parent_workspace_root: PathBuf,
    pub base_snapshot_id: String,
    pub operation_id: String,
    pub artifact_recorder: MutationEventRecorder,
}

/// Immutable, content-addressed parent baseline reusable by independent child worktrees.
#[derive(Debug, Clone)]
pub struct FrozenGitWorktreeBase {
    parent_workspace_root: PathBuf,
    base_snapshot: WorkspaceSnapshotBuild,
    base_commit: String,
    overlay_manifest: GitWorktreeOverlayManifest,
    overlay_digest: String,
    overlay_artifact_ref: MutationArtifactId,
    artifact_recorder: MutationEventRecorder,
}

impl FrozenGitWorktreeBase {
    #[must_use]
    pub fn parent_workspace_root(&self) -> &Path {
        &self.parent_workspace_root
    }

    #[must_use]
    pub fn base_snapshot_id(&self) -> &str {
        self.base_snapshot
            .workspace_snapshot_id
            .as_deref()
            .expect("frozen worktree base must own a complete snapshot")
    }

    #[must_use]
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    #[must_use]
    pub fn overlay_digest(&self) -> &str {
        &self.overlay_digest
    }

    #[must_use]
    pub fn overlay_artifact_ref(&self) -> &MutationArtifactId {
        &self.overlay_artifact_ref
    }

    #[must_use]
    pub fn overlay_content_artifact_refs(&self) -> Vec<MutationArtifactId> {
        self.overlay_manifest
            .entries
            .iter()
            .filter_map(|entry| entry.content_artifact_ref.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub fn overlay_entry_count(&self) -> usize {
        self.overlay_manifest.entries.len()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct GitWorktreeOverlayManifest {
    version: u32,
    base_commit: String,
    parent_snapshot_id: String,
    entries: Vec<GitWorktreeOverlayEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
struct GitWorktreeOverlayEntry {
    relative_path: PathBuf,
    action: GitWorktreeOverlayAction,
    source: GitWorktreeOverlaySource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_artifact_ref: Option<MutationArtifactId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mode: Option<u32>,
    #[serde(default)]
    readonly: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GitWorktreeOverlayAction {
    Upsert,
    Delete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GitWorktreeOverlaySource {
    TrackedDirty,
    Untracked,
}

/// Owned receipt for one materialized detached Git worktree.
///
/// The receipt is intentionally not `Clone`: cleanup consumes it so one runtime owner remains
/// responsible for the physical workspace.
#[derive(Debug)]
pub struct MaterializedGitWorktree {
    parent_workspace_root: PathBuf,
    workspace_root: PathBuf,
    isolation_root: PathBuf,
    isolated_workspace_id: String,
    base_snapshot_id: String,
    child_snapshot_id: String,
    base_commit: String,
    baseline_tree: String,
    overlay_digest: Option<String>,
    overlay_artifact_ref: Option<MutationArtifactId>,
    overlay_content_artifact_refs: Vec<MutationArtifactId>,
    overlay_entry_count: usize,
}

impl MaterializedGitWorktree {
    #[must_use]
    pub fn parent_workspace_root(&self) -> &Path {
        &self.parent_workspace_root
    }

    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub fn isolated_workspace_id(&self) -> &str {
        &self.isolated_workspace_id
    }

    #[must_use]
    pub fn base_snapshot_id(&self) -> &str {
        &self.base_snapshot_id
    }

    #[must_use]
    pub fn child_snapshot_id(&self) -> &str {
        &self.child_snapshot_id
    }

    #[must_use]
    pub fn base_commit(&self) -> &str {
        &self.base_commit
    }

    #[must_use]
    pub fn overlay_digest(&self) -> Option<&str> {
        self.overlay_digest.as_deref()
    }

    #[must_use]
    pub fn overlay_artifact_ref(&self) -> Option<&MutationArtifactId> {
        self.overlay_artifact_ref.as_ref()
    }

    #[must_use]
    pub fn overlay_content_artifact_refs(&self) -> &[MutationArtifactId] {
        &self.overlay_content_artifact_refs
    }

    #[must_use]
    pub fn overlay_entry_count(&self) -> usize {
        self.overlay_entry_count
    }

    /// Captures the current complete task verification snapshot for this exact worktree.
    ///
    /// # Errors
    ///
    /// Returns an error when the worktree can no longer be snapshotted completely.
    pub async fn current_snapshot_id(&self) -> Result<String> {
        task_workspace_snapshot(self.workspace_root.clone())
            .await?
            .workspace_snapshot_id
            .ok_or_else(|| anyhow!("isolated worktree snapshot is incomplete"))
    }

    /// Removes this exact worktree through Git and returns a bounded cleanup receipt.
    ///
    /// # Errors
    ///
    /// Returns an error if the receipt no longer resolves inside its frozen isolation root or Git
    /// cannot remove the worktree. The function never recursively deletes an arbitrary path.
    pub async fn cleanup(self) -> Result<GitWorktreeCleanupReceipt> {
        cleanup_owned_git_worktree(
            &self.parent_workspace_root,
            &self.isolation_root,
            &self.workspace_root,
            &self.isolated_workspace_id,
        )
        .await
    }

    /// Extracts one bounded text changeset from this exact worktree.
    ///
    /// The child must keep `HEAD` at the materialized base commit. Ignored build/cache output is
    /// excluded by Git; non-ignored untracked files are added with intent-to-add so the artifact
    /// includes their content without committing or mutating the parent repository index.
    ///
    /// # Errors
    ///
    /// Returns an error for ref drift, symlinks, special files, binary content, unsafe paths,
    /// empty changes, or any file/artifact budget overflow.
    pub async fn extract_changeset(
        &self,
        change_set_id: ChangeSetId,
        title: impl Into<String>,
        summary: impl Into<String>,
    ) -> Result<Option<TaskChildChangeSetProposal>> {
        extract_git_worktree_changeset(self, change_set_id, title.into(), summary.into()).await
    }
}

/// Physical cleanup result for one materialized Git worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeCleanupReceipt {
    pub isolated_workspace_id: String,
    pub workspace_root: PathBuf,
    pub isolation_root_removed: bool,
    pub status: IsolatedWorkspaceCleanupStatus,
}

/// Request to reconcile one exact runtime-owned Git worktree from durable identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeCleanupRequest {
    pub parent_workspace_root: PathBuf,
    pub isolated_workspace_id: String,
}

/// Bounded startup cleanup summary. Individual failures remain durable inventory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IsolatedWorkspaceCleanupReconciliation {
    pub inspected: usize,
    pub removed: usize,
    pub already_missing: usize,
    pub failed: usize,
    pub failures: Vec<String>,
}

/// Freezes the exact parent snapshot and persists a safe dirty/untracked overlay through the
/// RFC-0002 content-addressed mutation artifact store.
///
/// The caller should hold the workspace mutation lease while this function runs. The function
/// independently checks the requested snapshot before and after capture so mutations that do not
/// participate in that lease still fail closed.
///
/// # Errors
///
/// Returns an error before child materialization for snapshot/ref drift, submodules, unmerged
/// paths, unsafe or secret-like overlay content, unsupported file kinds, or artifact budgets.
pub async fn freeze_git_worktree_base(
    request: GitWorktreeBaseFreezeRequest,
) -> Result<FrozenGitWorktreeBase> {
    if request.base_snapshot_id.trim().is_empty() {
        bail!("isolated Git worktree base snapshot id must not be empty");
    }
    if request.operation_id.trim().is_empty() {
        bail!("isolated Git worktree overlay operation id must not be empty");
    }
    let parent_workspace_root = canonical_directory(&request.parent_workspace_root)
        .await
        .context("failed to resolve parent workspace root for isolated Git worktree overlay")?;
    validate_git_repository_root(&parent_workspace_root).await?;
    validate_no_submodules(&parent_workspace_root).await?;
    let parent_snapshot =
        validate_parent_snapshot(&parent_workspace_root, &request.base_snapshot_id).await?;
    let base_commit = resolve_base_commit(&parent_workspace_root).await?;
    let workspace_id = stable_workspace_id(&parent_workspace_root)?;
    let overlay_manifest = capture_git_worktree_overlay(
        &parent_workspace_root,
        &parent_snapshot,
        &base_commit,
        &workspace_id,
        &request.operation_id,
        &request.artifact_recorder,
    )
    .await?;
    validate_frozen_parent(&parent_workspace_root, &base_commit, &parent_snapshot).await?;
    let manifest_bytes = serde_json::to_vec(&overlay_manifest)
        .context("failed to encode isolated Git worktree overlay manifest")?;
    let overlay_digest = format!("sha256:{}", bytes_sha256(&manifest_bytes));
    let overlay_artifact_ref = capture_immutable_overlay_artifact(
        request.artifact_recorder.clone(),
        workspace_id,
        request.operation_id.clone(),
        PathBuf::from(".sigil-overlay/manifest.json"),
        manifest_bytes,
    )
    .await?;
    Ok(FrozenGitWorktreeBase {
        parent_workspace_root,
        base_snapshot: parent_snapshot,
        base_commit,
        overlay_manifest,
        overlay_digest,
        overlay_artifact_ref,
        artifact_recorder: request.artifact_recorder,
    })
}

/// Materializes a detached Git worktree only when the parent is clean and still matches the
/// requested workspace snapshot.
///
/// The destination is derived from the canonical Git common directory plus a validated opaque
/// workspace id. It never accepts a caller-provided destination path.
///
/// # Errors
///
/// Returns an error before worktree creation when the parent is not a repository root, is dirty,
/// contains submodules, has drifted from `base_snapshot_id`, or the isolated id is unsafe. A
/// post-checkout snapshot mismatch triggers a best-effort Git-owned rollback and still fails
/// closed.
pub async fn materialize_git_worktree(
    request: GitWorktreeMaterializationRequest,
) -> Result<MaterializedGitWorktree> {
    validate_isolated_workspace_id(&request.isolated_workspace_id)?;
    if request.base_snapshot_id.trim().is_empty() {
        bail!("isolated Git worktree base snapshot id must not be empty");
    }
    let parent_workspace_root = canonical_directory(&request.parent_workspace_root)
        .await
        .context("failed to resolve parent workspace root for isolated Git worktree")?;
    validate_git_repository_root(&parent_workspace_root).await?;
    validate_clean_parent(&parent_workspace_root).await?;
    let parent_snapshot =
        validate_parent_snapshot(&parent_workspace_root, &request.base_snapshot_id).await?;
    let base_commit = resolve_base_commit(&parent_workspace_root).await?;
    materialize_git_worktree_binding(
        parent_workspace_root,
        request.isolated_workspace_id,
        request.base_snapshot_id,
        parent_snapshot,
        base_commit,
        None,
    )
    .await
}

/// Materializes one independently owned worktree from a frozen commit plus immutable overlay.
///
/// Each call creates a separate worktree and consumes no mutable shared directory. Content blobs
/// may be shared read-only through the RFC-0002 artifact store.
///
/// # Errors
///
/// Returns an error and cleans up the private worktree if the parent snapshot/ref drifts, an
/// overlay artifact is unavailable, overlay application is unsafe, or the materialized snapshot
/// differs from the frozen parent snapshot.
pub async fn materialize_git_worktree_from_frozen_base(
    frozen: &FrozenGitWorktreeBase,
    isolated_workspace_id: impl Into<String>,
) -> Result<MaterializedGitWorktree> {
    let isolated_workspace_id = isolated_workspace_id.into();
    validate_isolated_workspace_id(&isolated_workspace_id)?;
    validate_frozen_parent(
        &frozen.parent_workspace_root,
        &frozen.base_commit,
        &frozen.base_snapshot,
    )
    .await?;
    materialize_git_worktree_binding(
        frozen.parent_workspace_root.clone(),
        isolated_workspace_id,
        frozen.base_snapshot_id().to_owned(),
        frozen.base_snapshot.clone(),
        frozen.base_commit.clone(),
        Some(frozen),
    )
    .await
}

async fn materialize_git_worktree_binding(
    parent_workspace_root: PathBuf,
    isolated_workspace_id: String,
    base_snapshot_id: String,
    parent_snapshot: WorkspaceSnapshotBuild,
    base_commit: String,
    frozen: Option<&FrozenGitWorktreeBase>,
) -> Result<MaterializedGitWorktree> {
    let git_common_dir = resolve_git_common_dir(&parent_workspace_root).await?;
    let isolation_root = prepare_isolation_root(&git_common_dir).await?;
    let workspace_root = isolation_root.join(&isolated_workspace_id);
    ensure_confined_destination(&isolation_root, &workspace_root, &isolated_workspace_id)?;
    if tokio::fs::symlink_metadata(&workspace_root).await.is_ok() {
        bail!(
            "isolated Git worktree destination already exists for {}",
            isolated_workspace_id
        );
    }

    let add_result = run_git(
        &parent_workspace_root,
        [
            OsString::from("worktree"),
            OsString::from("add"),
            OsString::from("--detach"),
            workspace_root.as_os_str().to_owned(),
            OsString::from(&base_commit),
        ],
    )
    .await;
    if let Err(error) = add_result {
        let cleanup_error =
            cleanup_failed_materialization(&parent_workspace_root, &workspace_root).await;
        return Err(with_cleanup_context(error, cleanup_error));
    }

    let canonical_workspace_root = match canonical_directory(&workspace_root).await {
        Ok(path) => path,
        Err(error) => {
            let cleanup_error =
                cleanup_failed_materialization(&parent_workspace_root, &workspace_root).await;
            return Err(with_cleanup_context(error, cleanup_error));
        }
    };
    if let Err(error) = ensure_confined_destination(
        &isolation_root,
        &canonical_workspace_root,
        &isolated_workspace_id,
    ) {
        let cleanup_error =
            cleanup_failed_materialization(&parent_workspace_root, &canonical_workspace_root).await;
        return Err(with_cleanup_context(error, cleanup_error));
    }
    if let Some(frozen) = frozen
        && let Err(error) = apply_frozen_overlay(&canonical_workspace_root, frozen).await
    {
        let cleanup_error =
            cleanup_failed_materialization(&parent_workspace_root, &canonical_workspace_root).await;
        return Err(with_cleanup_context(error, cleanup_error));
    }
    let child_snapshot_id =
        match validate_materialized_snapshot(&canonical_workspace_root, &parent_snapshot).await {
            Ok(snapshot_id) => snapshot_id,
            Err(error) => {
                let cleanup_error = cleanup_failed_materialization(
                    &parent_workspace_root,
                    &canonical_workspace_root,
                )
                .await;
                return Err(with_cleanup_context(error, cleanup_error));
            }
        };
    let baseline_tree = match capture_materialized_baseline_tree(&canonical_workspace_root).await {
        Ok(tree) => tree,
        Err(error) => {
            let cleanup_error =
                cleanup_failed_materialization(&parent_workspace_root, &canonical_workspace_root)
                    .await;
            return Err(with_cleanup_context(error, cleanup_error));
        }
    };
    if let Err(error) =
        validate_frozen_parent(&parent_workspace_root, &base_commit, &parent_snapshot).await
    {
        let cleanup_error =
            cleanup_failed_materialization(&parent_workspace_root, &canonical_workspace_root).await;
        return Err(with_cleanup_context(error, cleanup_error));
    }

    Ok(MaterializedGitWorktree {
        parent_workspace_root,
        workspace_root: canonical_workspace_root,
        isolation_root,
        isolated_workspace_id,
        base_snapshot_id,
        child_snapshot_id,
        base_commit,
        baseline_tree,
        overlay_digest: frozen.map(|frozen| frozen.overlay_digest.clone()),
        overlay_artifact_ref: frozen.map(|frozen| frozen.overlay_artifact_ref.clone()),
        overlay_content_artifact_refs: frozen
            .map(FrozenGitWorktreeBase::overlay_content_artifact_refs)
            .unwrap_or_default(),
        overlay_entry_count: frozen.map_or(0, FrozenGitWorktreeBase::overlay_entry_count),
    })
}

/// Removes an exact runtime-owned Git worktree reconstructed from durable identity.
///
/// # Errors
///
/// Returns an error if the id is unsafe, the owned root is not a regular confined directory, or
/// Git cannot remove the exact worktree. Missing roots/worktrees are successful terminal cleanup.
pub async fn cleanup_git_worktree(
    request: GitWorktreeCleanupRequest,
) -> Result<GitWorktreeCleanupReceipt> {
    validate_isolated_workspace_id(&request.isolated_workspace_id)?;
    let parent_workspace_root = canonical_directory(&request.parent_workspace_root)
        .await
        .context("failed to resolve parent workspace root for isolated Git worktree cleanup")?;
    validate_git_repository_root(&parent_workspace_root).await?;
    let git_common_dir = resolve_git_common_dir(&parent_workspace_root).await?;
    let isolation_root = git_common_dir.join(ISOLATED_WORKTREE_ROOT);
    let Some(isolation_root) = existing_isolation_root(&git_common_dir, &isolation_root).await?
    else {
        return Ok(GitWorktreeCleanupReceipt {
            isolated_workspace_id: request.isolated_workspace_id.clone(),
            workspace_root: isolation_root.join(&request.isolated_workspace_id),
            isolation_root_removed: true,
            status: IsolatedWorkspaceCleanupStatus::AlreadyMissing,
        });
    };
    let workspace_root = isolation_root.join(&request.isolated_workspace_id);
    cleanup_owned_git_worktree(
        &parent_workspace_root,
        &isolation_root,
        &workspace_root,
        &request.isolated_workspace_id,
    )
    .await
}

/// Reconciles all durable isolated-workspace cleanup inventory for the current parent workspace.
///
/// Binding conflicts, workspace mismatches, and unsupported backends fail closed without touching
/// a path. Every inspected item receives one append-only cleanup outcome; failed outcomes remain
/// in inventory for later review/retry.
pub async fn reconcile_isolated_workspace_cleanup(
    session: &mut Session,
    parent_workspace_root: &Path,
) -> Result<IsolatedWorkspaceCleanupReconciliation> {
    let parent_workspace_root = canonical_directory(parent_workspace_root)
        .await
        .context("failed to resolve parent workspace for isolated cleanup reconciliation")?;
    let parent_workspace_id = stable_workspace_id(&parent_workspace_root)?;
    let inventory = session
        .write_isolation_projection()
        .isolated_workspace_cleanup_inventory()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let mut report = IsolatedWorkspaceCleanupReconciliation::default();

    for state in inventory {
        report.inspected += 1;
        let binding = state
            .prepared
            .as_ref()
            .map(|entry| {
                (
                    &entry.parent_workspace_id,
                    entry.isolation_mode,
                    entry.backend,
                )
            })
            .or_else(|| {
                state.created.as_ref().map(|entry| {
                    (
                        &entry.parent_workspace_id,
                        entry.isolation_mode,
                        entry.backend,
                    )
                })
            });
        let result = match binding {
            None => Err(anyhow!(
                "durable isolated workspace has no ownership binding"
            )),
            Some(_) if !state.is_consistent() => Err(anyhow!(
                "durable isolated workspace binding is inconsistent"
            )),
            Some((bound_parent, _, _)) if bound_parent != &parent_workspace_id => Err(anyhow!(
                "durable isolated workspace belongs to a different parent workspace"
            )),
            Some((_, mode, _)) if mode != WriteIsolationMode::Worktree => Err(anyhow!(
                "durable isolated workspace cleanup requires worktree isolation"
            )),
            Some((_, _, backend)) if backend != IsolatedWorkspaceBackend::GitWorktree => Err(
                anyhow!("durable isolated workspace cleanup backend is unsupported"),
            ),
            Some(_) => cleanup_git_worktree(GitWorktreeCleanupRequest {
                parent_workspace_root: parent_workspace_root.clone(),
                isolated_workspace_id: state.isolated_workspace_id.clone(),
            })
            .await
            .map(|receipt| receipt.status),
        };
        let status = match result {
            Ok(IsolatedWorkspaceCleanupStatus::Removed) => {
                report.removed += 1;
                IsolatedWorkspaceCleanupStatus::Removed
            }
            Ok(IsolatedWorkspaceCleanupStatus::AlreadyMissing) => {
                report.already_missing += 1;
                IsolatedWorkspaceCleanupStatus::AlreadyMissing
            }
            Ok(status) => {
                report.failed += 1;
                report.failures.push(format!(
                    "{}: unexpected cleanup status {}",
                    state.isolated_workspace_id,
                    status.as_str()
                ));
                IsolatedWorkspaceCleanupStatus::Failed
            }
            Err(error) => {
                report.failed += 1;
                report
                    .failures
                    .push(format!("{}: {error:#}", state.isolated_workspace_id));
                IsolatedWorkspaceCleanupStatus::Failed
            }
        };
        session.append_control(ControlEntry::IsolatedWorkspaceCleanupRecorded(
            IsolatedWorkspaceCleanupRecorded {
                isolated_workspace_id: state.isolated_workspace_id,
                status,
            },
        ))?;
    }
    Ok(report)
}

async fn capture_git_worktree_overlay(
    parent_workspace_root: &Path,
    parent_snapshot: &WorkspaceSnapshotBuild,
    base_commit: &str,
    workspace_id: &str,
    operation_id: &str,
    artifact_recorder: &MutationEventRecorder,
) -> Result<GitWorktreeOverlayManifest> {
    let unmerged = git_bytes_with_limit(
        parent_workspace_root,
        [
            OsString::from("ls-files"),
            OsString::from("--unmerged"),
            OsString::from("-z"),
        ],
        MAX_CHANGESET_PATH_BYTES,
    )
    .await?;
    if !unmerged.is_empty() {
        bail!("isolated Git worktree overlay does not support unmerged paths");
    }

    let tracked_dirty = git_bytes_with_limit(
        parent_workspace_root,
        [
            OsString::from("diff"),
            OsString::from("--name-only"),
            OsString::from("-z"),
            OsString::from("--no-renames"),
            OsString::from("HEAD"),
            OsString::from("--"),
        ],
        MAX_CHANGESET_PATH_BYTES,
    )
    .await?;
    let untracked = git_bytes_with_limit(
        parent_workspace_root,
        [
            OsString::from("ls-files"),
            OsString::from("--others"),
            OsString::from("--exclude-standard"),
            OsString::from("-z"),
        ],
        MAX_CHANGESET_PATH_BYTES,
    )
    .await?;
    let mut sources = BTreeMap::new();
    for path in parse_nul_paths(&tracked_dirty)? {
        sources.insert(path, GitWorktreeOverlaySource::TrackedDirty);
    }
    for path in parse_nul_paths(&untracked)? {
        sources
            .entry(path)
            .or_insert(GitWorktreeOverlaySource::Untracked);
    }
    let materializable_paths = parent_snapshot
        .manifest
        .entries
        .iter()
        .map(|entry| entry.normalized_path.clone())
        .collect::<BTreeSet<_>>();
    let materializable_entry_count = sources
        .keys()
        .filter(|path| materializable_paths.contains(*path))
        .count();
    if materializable_entry_count > MAX_CHANGESET_FILES {
        bail!(
            "isolated Git worktree overlay contains {} entries, exceeding the {} entry limit",
            materializable_entry_count,
            MAX_CHANGESET_FILES
        );
    }
    let mut total_bytes = 0_usize;
    let mut entries = Vec::with_capacity(sources.len());
    for (path, source) in sources {
        let relative_path = validate_relative_path(&path)?;
        validate_overlay_path(&relative_path)?;
        if !materializable_paths.contains(&relative_path) {
            continue;
        }
        reject_nested_repository(parent_workspace_root, &relative_path).await?;
        let absolute_path = parent_workspace_root.join(&relative_path);
        let metadata = match tokio::fs::symlink_metadata(&absolute_path).await {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect isolated overlay path {}",
                        relative_path.display()
                    )
                });
            }
        };
        let Some(metadata) = metadata else {
            if source == GitWorktreeOverlaySource::Untracked {
                bail!(
                    "untracked isolated overlay path disappeared during capture: {}",
                    relative_path.display()
                );
            }
            entries.push(GitWorktreeOverlayEntry {
                relative_path,
                action: GitWorktreeOverlayAction::Delete,
                source,
                content_artifact_ref: None,
                content_sha256: None,
                mode: None,
                readonly: false,
            });
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!(
                "isolated Git worktree overlay path must be a regular file: {}",
                relative_path.display()
            );
        }
        let file_size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        if file_size > MAX_CHANGESET_FILE_BYTES {
            bail!(
                "isolated Git worktree overlay path {} exceeds the {} byte file limit",
                relative_path.display(),
                MAX_CHANGESET_FILE_BYTES
            );
        }
        total_bytes = total_bytes.saturating_add(file_size);
        if total_bytes > MAX_OVERLAY_TOTAL_BYTES {
            bail!(
                "isolated Git worktree overlay exceeds the {} byte total limit",
                MAX_OVERLAY_TOTAL_BYTES
            );
        }
        let bytes = tokio::fs::read(&absolute_path).await.with_context(|| {
            format!(
                "failed to read isolated overlay path {}",
                relative_path.display()
            )
        })?;
        if looks_like_secret_overlay_content(&bytes) {
            bail!(
                "isolated Git worktree overlay rejected secret-like content at {}; use changeset-only isolation or clean the workspace first",
                relative_path.display()
            );
        }
        let content_sha256 = format!("sha256:{}", bytes_sha256(&bytes));
        let content_artifact_ref = capture_immutable_overlay_artifact(
            artifact_recorder.clone(),
            workspace_id.to_owned(),
            operation_id.to_owned(),
            relative_path.clone(),
            bytes,
        )
        .await?;
        entries.push(GitWorktreeOverlayEntry {
            relative_path,
            action: GitWorktreeOverlayAction::Upsert,
            source,
            content_artifact_ref: Some(content_artifact_ref),
            content_sha256: Some(content_sha256),
            mode: overlay_file_mode(&metadata),
            readonly: metadata.permissions().readonly(),
        });
    }
    entries.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(GitWorktreeOverlayManifest {
        version: OVERLAY_MANIFEST_VERSION,
        base_commit: base_commit.to_owned(),
        parent_snapshot_id: parent_snapshot
            .workspace_snapshot_id
            .clone()
            .ok_or_else(|| anyhow!("isolated overlay parent snapshot is incomplete"))?,
        entries,
    })
}

async fn apply_frozen_overlay(workspace_root: &Path, frozen: &FrozenGitWorktreeBase) -> Result<()> {
    let manifest_bytes = read_immutable_overlay_artifact(
        frozen.artifact_recorder.clone(),
        frozen.overlay_artifact_ref.clone(),
    )
    .await?;
    let observed_digest = format!("sha256:{}", bytes_sha256(&manifest_bytes));
    if observed_digest != frozen.overlay_digest {
        bail!("isolated Git worktree overlay manifest digest mismatch");
    }
    let observed_manifest = serde_json::from_slice::<GitWorktreeOverlayManifest>(&manifest_bytes)
        .context("failed to decode isolated Git worktree overlay manifest")?;
    if observed_manifest != frozen.overlay_manifest
        || observed_manifest.version != OVERLAY_MANIFEST_VERSION
        || observed_manifest.base_commit != frozen.base_commit
        || observed_manifest.parent_snapshot_id != frozen.base_snapshot_id()
    {
        bail!("isolated Git worktree overlay manifest binding mismatch");
    }

    for entry in &observed_manifest.entries {
        let relative_path = validate_relative_path(&entry.relative_path)?;
        validate_overlay_path(&relative_path)?;
        ensure_overlay_parent(workspace_root, &relative_path).await?;
        let absolute_path = workspace_root.join(&relative_path);
        let existing = match tokio::fs::symlink_metadata(&absolute_path).await {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("failed to inspect overlay destination"),
        };
        if existing
            .as_ref()
            .is_some_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
        {
            bail!(
                "isolated Git worktree overlay destination is not a regular file: {}",
                relative_path.display()
            );
        }
        match entry.action {
            GitWorktreeOverlayAction::Delete => {
                if existing.is_some() {
                    tokio::fs::remove_file(&absolute_path)
                        .await
                        .with_context(|| {
                            format!(
                                "failed to delete inherited overlay path {}",
                                relative_path.display()
                            )
                        })?;
                }
            }
            GitWorktreeOverlayAction::Upsert => {
                let artifact_ref = entry.content_artifact_ref.as_ref().ok_or_else(|| {
                    anyhow!(
                        "isolated overlay path {} is missing content artifact",
                        relative_path.display()
                    )
                })?;
                let bytes = read_immutable_overlay_artifact(
                    frozen.artifact_recorder.clone(),
                    artifact_ref.clone(),
                )
                .await?;
                let content_sha256 = format!("sha256:{}", bytes_sha256(&bytes));
                if entry.content_sha256.as_deref() != Some(content_sha256.as_str()) {
                    bail!(
                        "isolated overlay path {} content digest mismatch",
                        relative_path.display()
                    );
                }
                tokio::fs::write(&absolute_path, bytes)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to apply inherited overlay path {}",
                            relative_path.display()
                        )
                    })?;
                apply_overlay_permissions(&absolute_path, entry).await?;
            }
        }
    }
    Ok(())
}

async fn capture_immutable_overlay_artifact(
    recorder: MutationEventRecorder,
    workspace_id: String,
    operation_id: String,
    source_path: PathBuf,
    bytes: Vec<u8>,
) -> Result<MutationArtifactId> {
    tokio::task::spawn_blocking(move || {
        recorder.capture_immutable_content_artifact(
            &workspace_id,
            &operation_id,
            &source_path,
            &bytes,
        )
    })
    .await
    .context("isolated overlay artifact capture task failed")?
}

async fn read_immutable_overlay_artifact(
    recorder: MutationEventRecorder,
    artifact_id: MutationArtifactId,
) -> Result<Vec<u8>> {
    tokio::task::spawn_blocking(move || recorder.read_immutable_content_artifact(&artifact_id))
        .await
        .context("isolated overlay artifact read task failed")?
}

async fn capture_materialized_baseline_tree(workspace_root: &Path) -> Result<String> {
    run_git(
        workspace_root,
        [
            OsString::from("add"),
            OsString::from("--all"),
            OsString::from("--"),
        ],
    )
    .await?;
    let tree = git_text(workspace_root, [OsString::from("write-tree")])
        .await
        .context("failed to capture post-overlay baseline tree")?;
    validate_git_object_id("post-overlay baseline tree", &tree)?;
    run_git(
        workspace_root,
        [
            OsString::from("reset"),
            OsString::from("--mixed"),
            OsString::from("--quiet"),
            OsString::from("HEAD"),
        ],
    )
    .await?;
    Ok(tree)
}

async fn validate_frozen_parent(
    parent_workspace_root: &Path,
    base_commit: &str,
    parent_snapshot: &WorkspaceSnapshotBuild,
) -> Result<()> {
    let observed_commit = resolve_base_commit(parent_workspace_root).await?;
    if observed_commit != base_commit {
        bail!("parent Git HEAD drifted from the frozen worktree base commit");
    }
    let expected_snapshot_id = parent_snapshot
        .workspace_snapshot_id
        .as_deref()
        .ok_or_else(|| anyhow!("frozen parent workspace snapshot is incomplete"))?;
    let observed = validate_parent_snapshot(parent_workspace_root, expected_snapshot_id).await?;
    if observed.manifest.entries != parent_snapshot.manifest.entries
        || observed.manifest.scope_hash != parent_snapshot.manifest.scope_hash
    {
        bail!("parent workspace materializable manifest drifted after overlay freeze");
    }
    Ok(())
}

async fn resolve_base_commit(parent_workspace_root: &Path) -> Result<String> {
    let base_commit = git_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("HEAD^{commit}"),
        ],
    )
    .await
    .context("failed to resolve isolated Git worktree base commit")?;
    validate_git_object_id("base commit", &base_commit)?;
    Ok(base_commit)
}

fn validate_git_object_id(label: &str, object_id: &str) -> Result<()> {
    if !(40..=64).contains(&object_id.len())
        || !object_id.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("Git returned an invalid {label}");
    }
    Ok(())
}

fn validate_overlay_path(relative_path: &Path) -> Result<()> {
    if is_sensitive_mutation_artifact_path(relative_path) {
        bail!(
            "isolated Git worktree overlay rejected secret-like path {}; use changeset-only isolation or clean the workspace first",
            relative_path.display()
        );
    }
    Ok(())
}

fn looks_like_secret_overlay_content(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let upper = text.to_ascii_uppercase();
    upper.contains("-----BEGIN PRIVATE KEY-----")
        || upper.contains("-----BEGIN RSA PRIVATE KEY-----")
        || upper.contains("-----BEGIN OPENSSH PRIVATE KEY-----")
        || upper.lines().any(line_contains_secret_assignment)
}

fn line_contains_secret_assignment(line: &str) -> bool {
    let line = line.trim();
    let Some((key, value)) = line.split_once(['=', ':']) else {
        return false;
    };
    let key = key
        .trim()
        .trim_matches(['"', '\''])
        .replace(['-', '.'], "_");
    const SECRET_KEY_MARKERS: &[&str] = &[
        "API_KEY",
        "APIKEY",
        "ACCESS_TOKEN",
        "AUTH_TOKEN",
        "AUTHORIZATION",
        "PASSWORD",
        "PRIVATE_KEY",
        "SECRET",
    ];
    (SECRET_KEY_MARKERS.iter().any(|marker| key.contains(marker))
        || secret_value_has_known_prefix(value))
        && likely_non_placeholder_secret(value)
}

fn secret_value_has_known_prefix(value: &str) -> bool {
    let value = value.trim().trim_matches(['"', '\'']).to_ascii_lowercase();
    [
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "akia",
        "aiza",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

fn likely_non_placeholder_secret(value: &str) -> bool {
    let value = value.trim().trim_matches(['"', '\'']);
    value.len() >= 16
        && !value.to_ascii_lowercase().contains("example")
        && !value.to_ascii_lowercase().contains("placeholder")
        && !value.contains("${")
}

async fn reject_nested_repository(workspace_root: &Path, relative_path: &Path) -> Result<()> {
    let mut current = relative_path.parent();
    while let Some(parent) = current {
        if parent.as_os_str().is_empty() {
            break;
        }
        if tokio::fs::symlink_metadata(workspace_root.join(parent).join(".git"))
            .await
            .is_ok()
        {
            bail!(
                "isolated Git worktree overlay does not support nested repository path {}",
                relative_path.display()
            );
        }
        current = parent.parent();
    }
    Ok(())
}

async fn ensure_overlay_parent(workspace_root: &Path, relative_path: &Path) -> Result<()> {
    let Some(parent) = relative_path.parent() else {
        return Ok(());
    };
    let mut current = workspace_root.to_path_buf();
    for component in parent.components() {
        let Component::Normal(name) = component else {
            bail!("isolated overlay parent contains unsafe traversal");
        };
        current.push(name);
        match tokio::fs::symlink_metadata(&current).await {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => bail!(
                "isolated overlay parent is not a plain directory: {}",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tokio::fs::create_dir(&current).await.with_context(|| {
                    format!(
                        "failed to create isolated overlay parent {}",
                        current.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect isolated overlay parent {}",
                        current.display()
                    )
                });
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn overlay_file_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    Some(metadata.permissions().mode() & 0o7777)
}

#[cfg(not(unix))]
fn overlay_file_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

#[cfg(unix)]
async fn apply_overlay_permissions(path: &Path, entry: &GitWorktreeOverlayEntry) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = entry
        .mode
        .unwrap_or(if entry.readonly { 0o444 } else { 0o644 });
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn apply_overlay_permissions(path: &Path, entry: &GitWorktreeOverlayEntry) -> Result<()> {
    let mut permissions = tokio::fs::metadata(path).await?.permissions();
    permissions.set_readonly(entry.readonly);
    tokio::fs::set_permissions(path, permissions).await?;
    Ok(())
}

async fn extract_git_worktree_changeset(
    materialized: &MaterializedGitWorktree,
    change_set_id: ChangeSetId,
    title: String,
    summary: String,
) -> Result<Option<TaskChildChangeSetProposal>> {
    let observed_head = git_text(
        &materialized.workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("HEAD^{commit}"),
        ],
    )
    .await?;
    if observed_head != materialized.base_commit {
        bail!("isolated Git worktree HEAD drifted from its bound base commit");
    }

    let untracked = git_bytes_with_limit(
        &materialized.workspace_root,
        [
            OsString::from("ls-files"),
            OsString::from("--others"),
            OsString::from("--exclude-standard"),
            OsString::from("-z"),
        ],
        MAX_CHANGESET_PATH_BYTES,
    )
    .await?;
    let untracked = parse_nul_paths(&untracked)?;
    if untracked.len() > MAX_CHANGESET_FILES {
        bail!(
            "isolated changeset contains {} untracked files, exceeding the {} file limit",
            untracked.len(),
            MAX_CHANGESET_FILES
        );
    }
    for path in &untracked {
        validate_changed_file(&materialized.workspace_root, path, true).await?;
    }
    if !untracked.is_empty() {
        let mut args = vec![
            OsString::from("add"),
            OsString::from("--intent-to-add"),
            OsString::from("--"),
        ];
        args.extend(untracked.iter().map(|path| path.as_os_str().to_owned()));
        run_git(&materialized.workspace_root, args).await?;
    }

    let changed = git_bytes_with_limit(
        &materialized.workspace_root,
        [
            OsString::from("diff"),
            OsString::from("--name-only"),
            OsString::from("-z"),
            OsString::from("--no-renames"),
            OsString::from(&materialized.baseline_tree),
            OsString::from("--"),
        ],
        MAX_CHANGESET_PATH_BYTES,
    )
    .await?;
    let changed = parse_nul_paths(&changed)?;
    if changed.is_empty() {
        return Ok(None);
    }
    if changed.len() > MAX_CHANGESET_FILES {
        bail!(
            "isolated changeset contains {} files, exceeding the {} file limit",
            changed.len(),
            MAX_CHANGESET_FILES
        );
    }

    let mut files = Vec::with_capacity(changed.len());
    for path in &changed {
        let relative = validate_relative_path(path)?;
        let before = git_blob_at_tree(
            &materialized.workspace_root,
            &materialized.baseline_tree,
            &relative,
            MAX_CHANGESET_FILE_BYTES,
        )
        .await?;
        let after = read_changed_file(
            &materialized.workspace_root,
            &relative,
            MAX_CHANGESET_FILE_BYTES,
        )
        .await?;
        let action = match (before.is_some(), after.is_some()) {
            (false, true) => ChangeSetFileAction::Create,
            (true, true) => ChangeSetFileAction::Update,
            (true, false) => ChangeSetFileAction::Delete,
            (false, false) => {
                bail!(
                    "isolated changeset path {} has neither base nor child content",
                    relative.display()
                )
            }
        };
        if before.as_deref().is_some_and(is_binary_content)
            || after.as_deref().is_some_and(is_binary_content)
        {
            bail!(
                "isolated changeset path {} contains binary content",
                relative.display()
            );
        }
        let path_text = relative
            .to_str()
            .ok_or_else(|| anyhow!("isolated changeset path is not valid UTF-8"))?
            .replace('\\', "/");
        files.push(ChangeSetFile {
            path: path_text,
            previous_path: None,
            action,
            risk: ChangeSetRisk::Medium,
            before_hash: before.as_deref().map(bytes_sha256),
            after_hash: after.as_deref().map(bytes_sha256),
            diff_hash: None,
            additions: 0,
            deletions: 0,
            validations: isolated_file_validations(),
        });
    }

    let artifact_bytes = git_bytes_with_limit(
        &materialized.workspace_root,
        [
            OsString::from("diff"),
            OsString::from("--no-color"),
            OsString::from("--full-index"),
            OsString::from("--no-ext-diff"),
            OsString::from("--no-renames"),
            OsString::from(&materialized.baseline_tree),
            OsString::from("--"),
        ],
        MAX_CHANGESET_ARTIFACT_BYTES,
    )
    .await?;
    let artifact_content = String::from_utf8(artifact_bytes)
        .context("isolated changeset artifact is not valid UTF-8 text")?;
    if artifact_content.trim().is_empty() {
        bail!("isolated worktree produced an empty diff artifact");
    }
    let content_sha256 = bytes_sha256(artifact_content.as_bytes());
    let child_snapshot_id = task_workspace_snapshot(materialized.workspace_root.clone())
        .await?
        .workspace_snapshot_id
        .ok_or_else(|| anyhow!("isolated child snapshot is incomplete after changes"))?;
    let change_set = ChangeSet {
        id: change_set_id,
        title,
        summary,
        risk: ChangeSetRisk::Medium,
        files,
        validations: isolated_file_validations(),
    };
    let artifact_ref = format!("inline:sha256:{content_sha256}");
    let (declared_effect, observed_effects) = materialized_integration_effect(&change_set);
    let base_representation = match materialized.overlay_digest() {
        Some(overlay_digest) => IntegrationBaseRepresentation::SnapshotWorkspace {
            base_commit: materialized.base_commit().to_owned(),
            overlay_digest: overlay_digest.to_owned(),
        },
        None => IntegrationBaseRepresentation::CleanCommit {
            base_commit: materialized.base_commit().to_owned(),
        },
    };
    let integration_facts = IntegrationProposalFacts::from_changeset(
        &change_set,
        base_representation,
        IntegrationContentClass::Text,
        declared_effect,
        observed_effects,
        artifact_ref.clone(),
        Vec::new(),
    )?;
    Ok(Some(TaskChildChangeSetProposal {
        change_set,
        artifact_ref,
        artifact: TaskChildChangeSetArtifact {
            media_type: "text/x-diff".to_owned(),
            content: artifact_content,
            content_sha256,
        },
        source_isolation: WriteIsolationMode::Worktree,
        child_snapshot_id: Some(child_snapshot_id),
        integration_facts,
    }))
}

fn materialized_integration_effect(
    change_set: &ChangeSet,
) -> (IntegrationEffect, Vec<IntegrationObservedEffect>) {
    let mut observed = BTreeSet::new();
    for file in &change_set.files {
        let path = Path::new(&file.path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if matches!(
            file_name,
            "Cargo.toml"
                | "Cargo.lock"
                | "package.json"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "bun.lock"
                | "bun.lockb"
                | "go.mod"
                | "go.sum"
                | "pyproject.toml"
                | "uv.lock"
                | "poetry.lock"
        ) {
            observed.insert(IntegrationObservedEffect::Package);
        }
        if file_name == "build.rs"
            || path.components().any(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .is_some_and(|part| matches!(part, "build" | "dist" | "target"))
            })
        {
            observed.insert(IntegrationObservedEffect::Build);
        }
        if path.components().any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(|part| matches!(part, "generated" | "gen"))
        }) || file_name.contains(".generated.")
        {
            observed.insert(IntegrationObservedEffect::SharedGeneratedRoot);
        }
    }
    let declared_effect = if observed.iter().any(|effect| {
        matches!(
            effect,
            IntegrationObservedEffect::Package | IntegrationObservedEffect::Build
        )
    }) {
        IntegrationEffect::Global
    } else if observed.contains(&IntegrationObservedEffect::SharedGeneratedRoot) {
        IntegrationEffect::GeneratedArtifacts
    } else {
        IntegrationEffect::Files
    };
    (declared_effect, observed.into_iter().collect())
}

async fn validate_git_repository_root(parent_workspace_root: &Path) -> Result<()> {
    let top_level = git_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--show-toplevel"),
        ],
    )
    .await
    .context("isolated worktree requires a non-bare Git working tree")?;
    let top_level = canonical_directory(Path::new(&top_level))
        .await
        .context("failed to canonicalize Git repository root")?;
    if top_level != parent_workspace_root {
        bail!(
            "isolated worktree requires workspace root {} to equal Git repository root {}",
            parent_workspace_root.display(),
            top_level.display()
        );
    }
    Ok(())
}

async fn validate_clean_parent(parent_workspace_root: &Path) -> Result<()> {
    let status = git_bytes(
        parent_workspace_root,
        [
            OsString::from("status"),
            OsString::from("--porcelain=v1"),
            OsString::from("-z"),
            OsString::from("--untracked-files=all"),
        ],
    )
    .await
    .context("failed to inspect parent Git worktree status")?;
    if !status.is_empty() {
        bail!("isolated Git worktree requires a clean parent workspace");
    }
    validate_no_submodules(parent_workspace_root).await
}

async fn validate_no_submodules(parent_workspace_root: &Path) -> Result<()> {
    let submodules = git_bytes(
        parent_workspace_root,
        [
            OsString::from("submodule"),
            OsString::from("status"),
            OsString::from("--recursive"),
        ],
    )
    .await
    .context("failed to inspect parent Git submodules")?;
    if !submodules.is_empty() {
        bail!("isolated Git worktree does not yet support repositories with submodules");
    }
    Ok(())
}

async fn validate_parent_snapshot(
    parent_workspace_root: &Path,
    expected: &str,
) -> Result<WorkspaceSnapshotBuild> {
    let observed = task_workspace_snapshot(parent_workspace_root.to_path_buf()).await?;
    if observed.workspace_snapshot_id.as_deref() != Some(expected) {
        bail!("parent workspace snapshot drifted before isolated worktree materialization");
    }
    Ok(observed)
}

async fn validate_materialized_snapshot(
    workspace_root: &Path,
    parent_snapshot: &WorkspaceSnapshotBuild,
) -> Result<String> {
    let observed = task_workspace_snapshot(workspace_root.to_path_buf()).await?;
    if observed.manifest.scope_hash != parent_snapshot.manifest.scope_hash
        || observed.manifest.entries != parent_snapshot.manifest.entries
    {
        bail!("materialized Git worktree does not match the requested parent snapshot");
    }
    observed
        .workspace_snapshot_id
        .ok_or_else(|| anyhow!("materialized Git worktree snapshot is incomplete"))
}

async fn task_workspace_snapshot(workspace_root: PathBuf) -> Result<WorkspaceSnapshotBuild> {
    tokio::task::spawn_blocking(move || {
        let workspace_id = stable_workspace_id(&workspace_root)?;
        let snapshot = build_workspace_snapshot(
            &workspace_root,
            workspace_id,
            &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
            0,
        )?;
        if snapshot.workspace_snapshot_id.is_none() {
            bail!("workspace snapshot is incomplete");
        }
        Ok(snapshot)
    })
    .await
    .context("isolated worktree snapshot task failed")?
}

async fn resolve_git_common_dir(parent_workspace_root: &Path) -> Result<PathBuf> {
    let common_dir = git_text(
        parent_workspace_root,
        [
            OsString::from("rev-parse"),
            OsString::from("--git-common-dir"),
        ],
    )
    .await?;
    let common_dir = PathBuf::from(common_dir);
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        parent_workspace_root.join(common_dir)
    };
    canonical_directory(&common_dir)
        .await
        .context("failed to canonicalize Git common directory")
}

async fn prepare_isolation_root(git_common_dir: &Path) -> Result<PathBuf> {
    let isolation_root = git_common_dir.join(ISOLATED_WORKTREE_ROOT);
    match tokio::fs::symlink_metadata(&isolation_root).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "isolated Git worktree root is not a regular directory: {}",
                    isolation_root.display()
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir(&isolation_root)
                .await
                .with_context(|| {
                    format!(
                        "failed to create isolated Git worktree root {}",
                        isolation_root.display()
                    )
                })?;
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect isolated Git worktree root {}",
                    isolation_root.display()
                )
            });
        }
    }
    let canonical = canonical_directory(&isolation_root).await?;
    if canonical.parent() != Some(git_common_dir) {
        bail!("isolated Git worktree root escaped the Git common directory");
    }
    Ok(canonical)
}

async fn existing_isolation_root(
    git_common_dir: &Path,
    isolation_root: &Path,
) -> Result<Option<PathBuf>> {
    let metadata = match tokio::fs::symlink_metadata(isolation_root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect isolated worktree root {}",
                    isolation_root.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "isolated Git worktree root is not a regular directory: {}",
            isolation_root.display()
        );
    }
    let canonical = canonical_directory(isolation_root).await?;
    if canonical.parent() != Some(git_common_dir) {
        bail!("isolated Git worktree root escaped the Git common directory");
    }
    Ok(Some(canonical))
}

async fn cleanup_owned_git_worktree(
    parent_workspace_root: &Path,
    isolation_root: &Path,
    workspace_root: &Path,
    isolated_workspace_id: &str,
) -> Result<GitWorktreeCleanupReceipt> {
    ensure_confined_destination(isolation_root, workspace_root, isolated_workspace_id)?;
    let status = match tokio::fs::symlink_metadata(workspace_root).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "isolated Git worktree is not a regular directory: {}",
                    workspace_root.display()
                );
            }
            let canonical_workspace_root = canonical_directory(workspace_root).await?;
            ensure_confined_destination(
                isolation_root,
                &canonical_workspace_root,
                isolated_workspace_id,
            )?;
            run_git(
                parent_workspace_root,
                [
                    OsString::from("worktree"),
                    OsString::from("remove"),
                    OsString::from("--force"),
                    canonical_workspace_root.as_os_str().to_owned(),
                ],
            )
            .await
            .with_context(|| {
                format!("failed to remove isolated Git worktree {isolated_workspace_id}")
            })?;
            IsolatedWorkspaceCleanupStatus::Removed
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            run_git(
                parent_workspace_root,
                [
                    OsString::from("worktree"),
                    OsString::from("prune"),
                    OsString::from("--expire"),
                    OsString::from("now"),
                ],
            )
            .await
            .context("failed to prune missing isolated Git worktree metadata")?;
            IsolatedWorkspaceCleanupStatus::AlreadyMissing
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect isolated Git worktree {}",
                    workspace_root.display()
                )
            });
        }
    };

    let isolation_root_removed = match tokio::fs::remove_dir(isolation_root).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => false,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to remove empty isolated worktree root {}",
                    isolation_root.display()
                )
            });
        }
    };
    Ok(GitWorktreeCleanupReceipt {
        isolated_workspace_id: isolated_workspace_id.to_owned(),
        workspace_root: workspace_root.to_path_buf(),
        isolation_root_removed,
        status,
    })
}

fn parse_nul_paths(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let mut unique = BTreeSet::new();
    for raw in bytes.split(|byte| *byte == 0).filter(|raw| !raw.is_empty()) {
        let text =
            std::str::from_utf8(raw).context("isolated changeset path is not valid UTF-8")?;
        let path = validate_relative_path(Path::new(text))?;
        if !unique.insert(path.clone()) {
            bail!(
                "isolated changeset returned duplicate path {}",
                path.display()
            );
        }
        paths.push(path);
    }
    Ok(paths)
}

fn validate_relative_path(path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        bail!(
            "isolated changeset path must be non-empty and relative: {}",
            path.display()
        );
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            _ => bail!(
                "isolated changeset path contains unsafe traversal: {}",
                path.display()
            ),
        }
    }
    Ok(normalized)
}

async fn validate_changed_file(
    workspace_root: &Path,
    relative_path: &Path,
    must_exist: bool,
) -> Result<()> {
    let content =
        read_changed_file(workspace_root, relative_path, MAX_CHANGESET_FILE_BYTES).await?;
    if must_exist && content.is_none() {
        bail!(
            "isolated changeset path disappeared before extraction: {}",
            relative_path.display()
        );
    }
    Ok(())
}

async fn read_changed_file(
    workspace_root: &Path,
    relative_path: &Path,
    limit: usize,
) -> Result<Option<Vec<u8>>> {
    let relative_path = validate_relative_path(relative_path)?;
    let absolute_path = workspace_root.join(&relative_path);
    let metadata = match tokio::fs::symlink_metadata(&absolute_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to inspect isolated changeset path {}",
                    relative_path.display()
                )
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "isolated changeset path must be a regular file: {}",
            relative_path.display()
        );
    }
    if metadata.len() > limit as u64 {
        bail!(
            "isolated changeset path {} exceeds the {} byte file limit",
            relative_path.display(),
            limit
        );
    }
    let canonical = tokio::fs::canonicalize(&absolute_path)
        .await
        .with_context(|| {
            format!(
                "failed to canonicalize isolated changeset path {}",
                relative_path.display()
            )
        })?;
    if !canonical.starts_with(workspace_root) {
        bail!(
            "isolated changeset path escaped its workspace: {}",
            relative_path.display()
        );
    }
    Ok(Some(tokio::fs::read(&canonical).await.with_context(
        || {
            format!(
                "failed to read isolated changeset path {}",
                relative_path.display()
            )
        },
    )?))
}

async fn git_blob_at_tree(
    workspace_root: &Path,
    treeish: &str,
    relative_path: &Path,
    limit: usize,
) -> Result<Option<Vec<u8>>> {
    validate_git_object_id("baseline tree", treeish)?;
    let relative_path = validate_relative_path(relative_path)?;
    let tree = git_bytes_with_limit(
        workspace_root,
        [
            OsString::from("ls-tree"),
            OsString::from("-z"),
            OsString::from(treeish),
            OsString::from("--"),
            relative_path.as_os_str().to_owned(),
        ],
        MAX_CHANGESET_PATH_BYTES,
    )
    .await?;
    if tree.is_empty() {
        return Ok(None);
    }
    let record = tree
        .strip_suffix(&[0])
        .ok_or_else(|| anyhow!("Git ls-tree output was not NUL terminated"))?;
    if record.contains(&0) {
        bail!("Git ls-tree returned multiple entries for one isolated path");
    }
    let tab = record
        .iter()
        .position(|byte| *byte == b'\t')
        .ok_or_else(|| anyhow!("Git ls-tree output is missing its path separator"))?;
    let header =
        std::str::from_utf8(&record[..tab]).context("Git ls-tree header is not valid UTF-8")?;
    let mut fields = header.split_whitespace();
    let mode = fields
        .next()
        .ok_or_else(|| anyhow!("Git ls-tree output is missing file mode"))?;
    let kind = fields
        .next()
        .ok_or_else(|| anyhow!("Git ls-tree output is missing object kind"))?;
    let object = fields
        .next()
        .ok_or_else(|| anyhow!("Git ls-tree output is missing object id"))?;
    if !matches!(mode, "100644" | "100755") || kind != "blob" {
        bail!(
            "isolated changeset base path {} is not a regular tracked file",
            relative_path.display()
        );
    }
    let recorded_path =
        std::str::from_utf8(&record[tab + 1..]).context("Git ls-tree path is not valid UTF-8")?;
    if Path::new(recorded_path) != relative_path {
        bail!("Git ls-tree returned a mismatched isolated path");
    }
    git_bytes_with_limit(
        workspace_root,
        [
            OsString::from("cat-file"),
            OsString::from("blob"),
            OsString::from(object),
        ],
        limit,
    )
    .await
    .map(Some)
}

fn bytes_sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_binary_content(bytes: &[u8]) -> bool {
    bytes.contains(&0) || std::str::from_utf8(bytes).is_err()
}

fn isolated_file_validations() -> Vec<ChangeSetValidation> {
    [
        ChangeSetValidationKind::Path,
        ChangeSetValidationKind::Hash,
        ChangeSetValidationKind::Symlink,
        ChangeSetValidationKind::Binary,
    ]
    .into_iter()
    .map(|kind| ChangeSetValidation {
        kind,
        status: ChangeSetValidationStatus::Passed,
        message: None,
    })
    .collect()
}

fn validate_isolated_workspace_id(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > MAX_ISOLATED_WORKSPACE_ID_BYTES {
        bail!(
            "isolated workspace id must contain between 1 and {} bytes",
            MAX_ISOLATED_WORKSPACE_ID_BYTES
        );
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("isolated workspace id contains unsafe path characters");
    }
    Ok(())
}

fn ensure_confined_destination(
    isolation_root: &Path,
    destination: &Path,
    isolated_workspace_id: &str,
) -> Result<()> {
    if destination.parent() != Some(isolation_root)
        || destination.file_name().and_then(|name| name.to_str()) != Some(isolated_workspace_id)
    {
        bail!("isolated Git worktree destination escaped its owned root");
    }
    Ok(())
}

async fn cleanup_failed_materialization(
    parent_workspace_root: &Path,
    workspace_root: &Path,
) -> Result<()> {
    run_git(
        parent_workspace_root,
        [
            OsString::from("worktree"),
            OsString::from("remove"),
            OsString::from("--force"),
            workspace_root.as_os_str().to_owned(),
        ],
    )
    .await
    .map(|_| ())
}

fn with_cleanup_context(error: anyhow::Error, cleanup_error: Result<()>) -> anyhow::Error {
    match cleanup_error {
        Ok(()) => error,
        Err(cleanup_error) => error.context(format!(
            "isolated Git worktree rollback was incomplete: {cleanup_error:#}"
        )),
    }
}

async fn canonical_directory(path: &Path) -> Result<PathBuf> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .with_context(|| format!("failed to inspect directory {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("path is not a regular directory: {}", path.display());
    }
    tokio::fs::canonicalize(path)
        .await
        .with_context(|| format!("failed to canonicalize directory {}", path.display()))
}

async fn git_text(current_dir: &Path, args: impl IntoIterator<Item = OsString>) -> Result<String> {
    let output = git_bytes(current_dir, args).await?;
    let text = String::from_utf8(output).context("Git output path was not valid UTF-8")?;
    let text = text.trim();
    if text.is_empty() {
        bail!("Git command returned an empty result");
    }
    Ok(text.to_owned())
}

async fn git_bytes(
    current_dir: &Path,
    args: impl IntoIterator<Item = OsString>,
) -> Result<Vec<u8>> {
    run_git(current_dir, args).await
}

async fn git_bytes_with_limit(
    current_dir: &Path,
    args: impl IntoIterator<Item = OsString>,
    stdout_limit: usize,
) -> Result<Vec<u8>> {
    run_git_with_limit(current_dir, args, stdout_limit).await
}

async fn run_git(current_dir: &Path, args: impl IntoIterator<Item = OsString>) -> Result<Vec<u8>> {
    run_git_with_limit(current_dir, args, GIT_OUTPUT_LIMIT).await
}

async fn run_git_with_limit(
    current_dir: &Path,
    args: impl IntoIterator<Item = OsString>,
    stdout_limit: usize,
) -> Result<Vec<u8>> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(current_dir)
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start Git command {}", display_git_args(&args)))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Git command stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Git command stderr pipe is unavailable"))?;
    let output = tokio::time::timeout(GIT_COMMAND_TIMEOUT, async move {
        let (stdout, stderr, status) = tokio::try_join!(
            read_bounded_output(stdout, stdout_limit),
            read_bounded_output(stderr, GIT_ERROR_OUTPUT_LIMIT),
            child.wait()
        )?;
        Ok::<_, std::io::Error>(BoundedGitOutput {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .map_err(|_| {
        anyhow!(
            "Git command timed out after {} seconds",
            GIT_COMMAND_TIMEOUT.as_secs()
        )
    })?
    .with_context(|| format!("failed to collect Git command {}", display_git_args(&args)))?;
    if output.stdout.truncated {
        bail!(
            "Git command {} exceeded the {} byte stdout limit",
            display_git_args(&args),
            stdout_limit
        );
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        let suffix = if output.stderr.truncated {
            " [truncated]"
        } else {
            ""
        };
        bail!(
            "Git command {} failed with status {}: {}{}",
            display_git_args(&args),
            output.status,
            stderr.trim(),
            suffix
        );
    }
    Ok(output.stdout.bytes)
}

pub(crate) async fn run_git_bytes(
    current_dir: &Path,
    args: impl IntoIterator<Item = OsString>,
    stdout_limit: usize,
) -> Result<Vec<u8>> {
    run_git_with_limit(current_dir, args, stdout_limit).await
}

pub(crate) async fn run_git_bytes_with_stdin(
    current_dir: &Path,
    args: impl IntoIterator<Item = OsString>,
    stdin: &[u8],
    stdout_limit: usize,
) -> Result<Vec<u8>> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(current_dir)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start Git command {}", display_git_args(&args)))?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Git command stdin pipe is unavailable"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Git command stdout pipe is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Git command stderr pipe is unavailable"))?;
    let input = stdin.to_vec();
    let output = tokio::time::timeout(GIT_COMMAND_TIMEOUT, async move {
        let writer = async move {
            use tokio::io::AsyncWriteExt;
            child_stdin.write_all(&input).await?;
            child_stdin.shutdown().await
        };
        let (_, stdout, stderr, status) = tokio::try_join!(
            writer,
            read_bounded_output(stdout, stdout_limit),
            read_bounded_output(stderr, GIT_ERROR_OUTPUT_LIMIT),
            child.wait()
        )?;
        Ok::<_, std::io::Error>(BoundedGitOutput {
            status,
            stdout,
            stderr,
        })
    })
    .await
    .map_err(|_| {
        anyhow!(
            "Git command timed out after {} seconds: {}",
            GIT_COMMAND_TIMEOUT.as_secs(),
            display_git_args(&args)
        )
    })?
    .with_context(|| format!("failed to collect Git command {}", display_git_args(&args)))?;
    if output.stdout.truncated {
        bail!(
            "Git command {} exceeded the {} byte stdout limit",
            display_git_args(&args),
            stdout_limit
        );
    }
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr.bytes);
        let suffix = if output.stderr.truncated {
            " [truncated]"
        } else {
            ""
        };
        bail!(
            "Git command {} failed with status {}: {}{}",
            display_git_args(&args),
            output.status,
            stderr.trim(),
            suffix
        );
    }
    Ok(output.stdout.bytes)
}

fn display_git_args(args: &[OsString]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

struct BoundedGitOutput {
    status: ExitStatus,
    stdout: BoundedBytes,
    stderr: BoundedBytes,
}

struct BoundedBytes {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded_output(
    mut reader: impl AsyncRead + Unpin,
    limit: usize,
) -> std::io::Result<BoundedBytes> {
    let mut bytes = Vec::with_capacity(limit.min(8 * 1024));
    let mut truncated = false;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let keep = remaining.min(read);
        bytes.extend_from_slice(&chunk[..keep]);
        truncated |= keep < read;
    }
    Ok(BoundedBytes { bytes, truncated })
}
