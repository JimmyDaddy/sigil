//! RFC-0014 write-isolation domain contract and projection skeleton.
//!
//! This module records write lease, isolated workspace, changeset output, and merge review facts.
//! It does not enforce scheduling or create worktrees; those behaviors are layered on top of this
//! append-only fact model in later RFC-0014 slices.

use std::{
    collections::BTreeMap,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetResult, ChangeSetResultStatus, DurableEventType, EventClass,
    MutationBatchId, MutationBatchStatus, MutationEventRecorder, MutationSubject, OperationId,
    Session, WorkspaceId, WorkspaceSnapshotId, bytes_hash, delete_file_with_mutation_in_batch,
    file_content_hash,
    session::{ControlEntry, SessionLogEntry},
    stable_event_uuid, write_file_with_mutation_in_batch,
};

pub type WriteIsolationAgentId = String;

/// Stable identifier for an exclusive shared-workspace write lease.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct WriteLeaseId(String);

impl WriteLeaseId {
    /// Creates a path-safe write lease identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("write lease id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for WriteLeaseId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Stable identifier for one parent merge review.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct MergeReviewId(String);

impl MergeReviewId {
    /// Creates a path-safe merge review identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when `value` is empty or contains path separators or unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("merge review id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MergeReviewId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Runtime write isolation mode requested for one writing actor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteIsolationMode {
    SharedWorkspaceExclusive,
    ChangesetOnly,
    Worktree,
}

impl WriteIsolationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SharedWorkspaceExclusive => "shared_workspace_exclusive",
            Self::ChangesetOnly => "changeset_only",
            Self::Worktree => "worktree",
        }
    }
}

/// Scope protected by a shared-workspace write lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteLeaseScope {
    Workspace,
    Subjects(Vec<MutationSubject>),
}

/// Terminal status for one write lease.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WriteLeaseReleaseStatus {
    Released,
    Completed,
    Cancelled,
    Interrupted,
    Stale,
}

impl WriteLeaseReleaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Stale => "stale",
        }
    }
}

/// Backend used to provide an isolated child workspace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedWorkspaceBackend {
    ChangesetOnly,
    GitWorktree,
    Overlay,
    Unknown,
}

impl IsolatedWorkspaceBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ChangesetOnly => "changeset_only",
            Self::GitWorktree => "git_worktree",
            Self::Overlay => "overlay",
            Self::Unknown => "unknown",
        }
    }
}

/// Parent decision for one child-produced changeset.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeDecision {
    Accepted,
    Rejected,
    Conflict,
    Cancelled,
}

impl MergeDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Conflict => "conflict",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Durable fact emitted when an actor acquires a shared-workspace write lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WriteLeaseAcquired {
    pub lease_id: WriteLeaseId,
    pub workspace_id: WorkspaceId,
    pub owner_agent_id: WriteIsolationAgentId,
    pub isolation_mode: WriteIsolationMode,
    pub scope: WriteLeaseScope,
}

/// Durable fact emitted when an actor releases a write lease.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WriteLeaseReleased {
    pub lease_id: WriteLeaseId,
    pub status: WriteLeaseReleaseStatus,
}

/// Durable fact emitted when a child isolated workspace is created.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IsolatedWorkspaceCreated {
    pub isolated_workspace_id: WorkspaceId,
    pub parent_workspace_id: WorkspaceId,
    pub owner_agent_id: WriteIsolationAgentId,
    pub isolation_mode: WriteIsolationMode,
    pub base_snapshot_id: WorkspaceSnapshotId,
    pub backend: IsolatedWorkspaceBackend,
}

/// Durable intent recorded before physical isolated-workspace materialization begins.
///
/// The fields are self-contained so restart recovery can derive the exact owned workspace even
/// when a crash happens before [`IsolatedWorkspaceCreated`] is appended.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IsolatedWorkspacePrepared {
    pub isolated_workspace_id: WorkspaceId,
    pub parent_workspace_id: WorkspaceId,
    pub owner_agent_id: WriteIsolationAgentId,
    pub isolation_mode: WriteIsolationMode,
    pub base_snapshot_id: WorkspaceSnapshotId,
    pub backend: IsolatedWorkspaceBackend,
}

/// Bounded cleanup outcome for one prepared or created isolated workspace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IsolatedWorkspaceCleanupStatus {
    Removed,
    AlreadyMissing,
    Retained,
    Failed,
}

impl IsolatedWorkspaceCleanupStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::AlreadyMissing => "already_missing",
            Self::Retained => "retained",
            Self::Failed => "failed",
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Removed | Self::AlreadyMissing)
    }
}

/// Durable cleanup result for one isolated workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IsolatedWorkspaceCleanupRecorded {
    pub isolated_workspace_id: WorkspaceId,
    pub status: IsolatedWorkspaceCleanupStatus,
}

/// Durable fact emitted when an isolated writer produces a changeset for parent review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IsolatedChangeSetProduced {
    pub changeset_id: ChangeSetId,
    pub owner_agent_id: WriteIsolationAgentId,
    pub base_snapshot_id: WorkspaceSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_snapshot_id: Option<WorkspaceSnapshotId>,
    pub source_isolation: WriteIsolationMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub touched_subjects: Vec<MutationSubject>,
}

/// Durable fact emitted when parent review is requested for one isolated changeset.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MergeReviewRequested {
    pub review_id: MergeReviewId,
    pub changeset_id: ChangeSetId,
    pub parent_workspace_snapshot_id: WorkspaceSnapshotId,
}

/// Durable fact emitted when a parent merge review is resolved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MergeReviewResolved {
    pub review_id: MergeReviewId,
    pub decision: MergeDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Request to resolve one parent merge review and optionally apply the changeset to the parent.
#[derive(Debug, Clone)]
pub struct MergeReviewParentMutationRequest {
    pub review_id: MergeReviewId,
    pub decision: MergeDecision,
    pub reason: Option<String>,
    pub change_set: ChangeSet,
    pub artifact_content: String,
    pub workspace_root: PathBuf,
    pub tool_call_id: String,
}

/// Result of resolving a merge review through the parent mutation handoff path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReviewParentMutationOutcome {
    pub review_id: MergeReviewId,
    pub decision: MergeDecision,
    pub change_set_result: Option<ChangeSetResult>,
    pub batch_id: Option<MutationBatchId>,
    pub batch_status: Option<MutationBatchStatus>,
    pub committed_operations: Vec<OperationId>,
    pub failed_operations: Vec<OperationId>,
}

/// Resolves a merge review and applies accepted changesets through RFC-0002 mutation evidence.
///
/// Rejected, conflicted or cancelled decisions only append `MergeReviewResolved`. Accepted
/// decisions require a durable session store and apply each changeset file through a mutation batch.
/// The review artifact must contain a unified diff for the files being applied.
///
/// # Errors
///
/// Returns an error when the review is unknown, already resolved, targets a different changeset,
/// the session is not durable for an accepted merge, or the review artifact cannot be applied.
pub fn resolve_merge_review_parent_mutation(
    session: &mut Session,
    request: MergeReviewParentMutationRequest,
) -> Result<MergeReviewParentMutationOutcome> {
    let projection = session.write_isolation_projection();
    let review = projection
        .merge_reviews
        .get(&request.review_id)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "merge review {} is not recorded",
                request.review_id.as_str()
            )
        })?;
    let requested = review.requested.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "merge review {} has no request record",
            request.review_id.as_str()
        )
    })?;
    if review.resolved.is_some() {
        bail!(
            "merge review {} is already resolved",
            request.review_id.as_str()
        );
    }
    if requested.changeset_id != request.change_set.id {
        bail!(
            "merge review {} targets changeset {}, not {}",
            request.review_id.as_str(),
            requested.changeset_id.as_str(),
            request.change_set.id.as_str()
        );
    }

    if request.decision != MergeDecision::Accepted {
        session.append_control(ControlEntry::MergeReviewResolved(MergeReviewResolved {
            review_id: request.review_id.clone(),
            decision: request.decision,
            reason: request.reason,
        }))?;
        return Ok(MergeReviewParentMutationOutcome {
            review_id: request.review_id,
            decision: request.decision,
            change_set_result: None,
            batch_id: None,
            batch_status: None,
            committed_operations: Vec::new(),
            failed_operations: Vec::new(),
        });
    }

    let recorder = session
        .mutation_event_recorder()
        .ok_or_else(|| anyhow::anyhow!("accepted merge review requires a durable session store"))?;
    let workspace_root = canonical_workspace_root(&request.workspace_root)?;
    let artifact = UnifiedDiffArtifact::parse(&request.artifact_content)?;
    let batch_id = changeset_merge_batch_id(&request.review_id, &request.change_set.id);
    let batch_operation_id =
        changeset_merge_operation_id(&request.review_id, &request.change_set.id);
    let expected_subjects = changeset_touched_subjects(&request.change_set)?;

    session.append_control(ControlEntry::MergeReviewResolved(MergeReviewResolved {
        review_id: request.review_id.clone(),
        decision: MergeDecision::Accepted,
        reason: request.reason,
    }))?;
    recorder.append_batch_started(&batch_id, &batch_operation_id, &expected_subjects)?;

    let mut file_results = Vec::new();
    let mut committed_operations = Vec::new();
    let mut failed_operations = Vec::new();
    let mut latest_snapshot_id = None;

    for file in &request.change_set.files {
        match apply_changeset_file_to_parent(
            &recorder,
            &workspace_root,
            &request.tool_call_id,
            &batch_id,
            file,
            &artifact,
        ) {
            Ok(applied) => {
                latest_snapshot_id = Some(applied.workspace_snapshot_id);
                committed_operations.push(applied.operation_id);
                file_results.push(ChangeSetFileResult {
                    path: file.path.clone(),
                    action: file.action,
                    status: ChangeSetFileResultStatus::Applied,
                    message: None,
                    validations: Vec::new(),
                });
            }
            Err(error) => {
                let operation_id =
                    failed_changeset_file_operation_id(&batch_id, &file.path, file.action);
                failed_operations.push(operation_id);
                file_results.push(ChangeSetFileResult {
                    path: file.path.clone(),
                    action: file.action,
                    status: ChangeSetFileResultStatus::Failed,
                    message: Some(error.to_string()),
                    validations: Vec::new(),
                });
            }
        }
    }

    let batch_status = mutation_batch_status(&committed_operations, &failed_operations);
    recorder.append_batch_finished(
        &batch_id,
        batch_status,
        &committed_operations,
        &failed_operations,
    )?;
    let result = ChangeSetResult {
        id: request.change_set.id.clone(),
        status: changeset_result_status(batch_status),
        file_results,
        message: Some("parent merge applied through RFC-0002 mutation batch".to_owned()),
    };
    session.append_control(ControlEntry::ChangeSetApplied(result.clone()))?;
    if let Some(after_snapshot_id) = latest_snapshot_id {
        session.append_durable_event(
            DurableEventType::ChildChangesetMerged,
            EventClass::Critical,
            json!({
                "review_id": request.review_id.as_str(),
                "changeset_id": request.change_set.id.as_str(),
                "batch_id": batch_id,
                "parent_workspace_snapshot_before_id": requested.parent_workspace_snapshot_id,
                "parent_workspace_snapshot_after_id": after_snapshot_id,
                "committed_operations": committed_operations,
                "failed_operations": failed_operations,
            }),
        )?;
    }

    Ok(MergeReviewParentMutationOutcome {
        review_id: request.review_id,
        decision: MergeDecision::Accepted,
        change_set_result: Some(result),
        batch_id: Some(batch_id),
        batch_status: Some(batch_status),
        committed_operations,
        failed_operations,
    })
}

/// Reconstructed write-isolation state from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WriteIsolationProjection {
    pub leases: BTreeMap<WriteLeaseId, WriteLeaseState>,
    pub active_leases_by_workspace: BTreeMap<WorkspaceId, WriteLeaseId>,
    pub isolated_workspaces: BTreeMap<WorkspaceId, IsolatedWorkspaceCreated>,
    pub isolated_workspace_states: BTreeMap<WorkspaceId, IsolatedWorkspaceState>,
    pub isolated_changesets: BTreeMap<ChangeSetId, IsolatedChangeSetProduced>,
    pub merge_reviews: BTreeMap<MergeReviewId, MergeReviewState>,
    pub replay_order: Vec<WriteIsolationRecordRef>,
}

impl WriteIsolationProjection {
    /// Replays append-only session entries into the latest write-isolation projection.
    pub fn from_entries(entries: &[SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let SessionLogEntry::Control(control) = entry {
                projection.apply_control_entry(control);
            }
        }
        projection
    }

    pub(crate) fn apply_control_entry(&mut self, control: &ControlEntry) {
        match control {
            ControlEntry::WriteLeaseAcquired(entry) => self.apply_lease_acquired(entry),
            ControlEntry::WriteLeaseReleased(entry) => self.apply_lease_released(entry),
            ControlEntry::IsolatedWorkspaceCreated(entry) => {
                self.apply_isolated_workspace_created(entry);
            }
            ControlEntry::IsolatedWorkspacePrepared(entry) => {
                self.apply_isolated_workspace_prepared(entry);
            }
            ControlEntry::IsolatedWorkspaceCleanupRecorded(entry) => {
                self.apply_isolated_workspace_cleanup(entry);
            }
            ControlEntry::IsolatedChangeSetProduced(entry) => {
                self.apply_isolated_changeset_produced(entry);
            }
            ControlEntry::MergeReviewRequested(entry) => self.apply_merge_review_requested(entry),
            ControlEntry::MergeReviewResolved(entry) => self.apply_merge_review_resolved(entry),
            _ => {}
        }
    }

    pub fn active_lease_for_workspace(&self, workspace_id: &str) -> Option<&WriteLeaseState> {
        self.active_leases_by_workspace
            .get(workspace_id)
            .and_then(|lease_id| self.leases.get(lease_id))
    }

    pub fn has_active_write_lease(&self, workspace_id: &str) -> bool {
        self.active_lease_for_workspace(workspace_id)
            .is_some_and(WriteLeaseState::is_active)
    }

    /// Returns prepared or created workspaces whose cleanup is incomplete or explicitly retained.
    #[must_use]
    pub fn isolated_workspace_cleanup_inventory(&self) -> Vec<&IsolatedWorkspaceState> {
        self.isolated_workspace_states
            .values()
            .filter(|state| state.requires_cleanup())
            .collect()
    }

    /// Fails closed when acquiring `entry` would create a second active shared-workspace writer.
    ///
    /// # Errors
    ///
    /// Returns an error if the requested lease is not a shared-workspace lease or if another active
    /// lease already owns the workspace.
    pub fn validate_can_acquire_shared_workspace_lease(
        &self,
        entry: &WriteLeaseAcquired,
    ) -> Result<()> {
        if entry.isolation_mode != WriteIsolationMode::SharedWorkspaceExclusive {
            bail!(
                "write lease {} must use shared_workspace_exclusive isolation",
                entry.lease_id.as_str()
            );
        }
        let Some(active) = self.active_lease_for_workspace(&entry.workspace_id) else {
            return Ok(());
        };
        if active.lease_id == entry.lease_id {
            return Ok(());
        }
        let owner = active
            .acquired
            .as_ref()
            .map(|lease| lease.owner_agent_id.as_str())
            .unwrap_or("unknown");
        bail!(
            "workspace {} already has active write lease {} owned by {}",
            entry.workspace_id,
            active.lease_id.as_str(),
            owner
        )
    }

    /// Builds stale release records for currently active leases.
    ///
    /// The caller is responsible for deciding that the owning run is stale or interrupted before
    /// appending these records. Normal acquisition does not auto-release another active owner.
    #[must_use]
    pub fn stale_active_lease_releases(&self) -> Vec<WriteLeaseReleased> {
        self.active_leases_by_workspace
            .values()
            .filter_map(|lease_id| self.leases.get(lease_id))
            .filter(|state| state.is_active())
            .map(|state| WriteLeaseReleased {
                lease_id: state.lease_id.clone(),
                status: WriteLeaseReleaseStatus::Stale,
            })
            .collect()
    }

    fn apply_lease_acquired(&mut self, entry: &WriteLeaseAcquired) {
        self.replay_order.push(WriteIsolationRecordRef::WriteLease {
            lease_id: entry.lease_id.clone(),
        });
        self.active_leases_by_workspace
            .insert(entry.workspace_id.clone(), entry.lease_id.clone());
        let state = self
            .leases
            .entry(entry.lease_id.clone())
            .or_insert_with(|| WriteLeaseState::new(entry.lease_id.clone()));
        state.acquired = Some(entry.clone());
        state.released = None;
    }

    fn apply_lease_released(&mut self, entry: &WriteLeaseReleased) {
        self.replay_order.push(WriteIsolationRecordRef::WriteLease {
            lease_id: entry.lease_id.clone(),
        });
        let state = self
            .leases
            .entry(entry.lease_id.clone())
            .or_insert_with(|| WriteLeaseState::new(entry.lease_id.clone()));
        let workspace_id = state
            .acquired
            .as_ref()
            .map(|acquired| acquired.workspace_id.clone());
        state.released = Some(entry.clone());
        if let Some(workspace_id) = workspace_id
            && self.active_leases_by_workspace.get(&workspace_id) == Some(&entry.lease_id)
        {
            self.active_leases_by_workspace.remove(&workspace_id);
        }
    }

    fn apply_isolated_workspace_created(&mut self, entry: &IsolatedWorkspaceCreated) {
        self.replay_order
            .push(WriteIsolationRecordRef::IsolatedWorkspace {
                workspace_id: entry.isolated_workspace_id.clone(),
            });
        self.isolated_workspaces
            .insert(entry.isolated_workspace_id.clone(), entry.clone());
        let state = self
            .isolated_workspace_states
            .entry(entry.isolated_workspace_id.clone())
            .or_insert_with(|| IsolatedWorkspaceState::new(entry.isolated_workspace_id.clone()));
        if state
            .created
            .as_ref()
            .is_some_and(|created| created != entry)
            || state
                .prepared
                .as_ref()
                .is_some_and(|prepared| !prepared_matches_created(prepared, entry))
        {
            state.binding_conflict = true;
        }
        state.created = Some(entry.clone());
    }

    fn apply_isolated_workspace_prepared(&mut self, entry: &IsolatedWorkspacePrepared) {
        self.replay_order
            .push(WriteIsolationRecordRef::IsolatedWorkspace {
                workspace_id: entry.isolated_workspace_id.clone(),
            });
        let state = self
            .isolated_workspace_states
            .entry(entry.isolated_workspace_id.clone())
            .or_insert_with(|| IsolatedWorkspaceState::new(entry.isolated_workspace_id.clone()));
        if state
            .prepared
            .as_ref()
            .is_some_and(|prepared| prepared != entry)
            || state
                .created
                .as_ref()
                .is_some_and(|created| !prepared_matches_created(entry, created))
        {
            state.binding_conflict = true;
        }
        state.prepared = Some(entry.clone());
    }

    fn apply_isolated_workspace_cleanup(&mut self, entry: &IsolatedWorkspaceCleanupRecorded) {
        self.replay_order
            .push(WriteIsolationRecordRef::IsolatedWorkspace {
                workspace_id: entry.isolated_workspace_id.clone(),
            });
        let state = self
            .isolated_workspace_states
            .entry(entry.isolated_workspace_id.clone())
            .or_insert_with(|| IsolatedWorkspaceState::new(entry.isolated_workspace_id.clone()));
        state.cleanup = Some(entry.clone());
    }

    fn apply_isolated_changeset_produced(&mut self, entry: &IsolatedChangeSetProduced) {
        self.replay_order
            .push(WriteIsolationRecordRef::IsolatedChangeSet {
                changeset_id: entry.changeset_id.clone(),
            });
        self.isolated_changesets
            .insert(entry.changeset_id.clone(), entry.clone());
    }

    fn apply_merge_review_requested(&mut self, entry: &MergeReviewRequested) {
        self.replay_order
            .push(WriteIsolationRecordRef::MergeReview {
                review_id: entry.review_id.clone(),
            });
        let state = self
            .merge_reviews
            .entry(entry.review_id.clone())
            .or_insert_with(|| MergeReviewState::new(entry.review_id.clone()));
        state.requested = Some(entry.clone());
        state.resolved = None;
    }

    fn apply_merge_review_resolved(&mut self, entry: &MergeReviewResolved) {
        self.replay_order
            .push(WriteIsolationRecordRef::MergeReview {
                review_id: entry.review_id.clone(),
            });
        let state = self
            .merge_reviews
            .entry(entry.review_id.clone())
            .or_insert_with(|| MergeReviewState::new(entry.review_id.clone()));
        state.resolved = Some(entry.clone());
    }
}

/// Latest projected state for one write lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteLeaseState {
    pub lease_id: WriteLeaseId,
    pub acquired: Option<WriteLeaseAcquired>,
    pub released: Option<WriteLeaseReleased>,
}

/// Latest append-only lifecycle state for one physical isolated workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolatedWorkspaceState {
    pub isolated_workspace_id: WorkspaceId,
    pub prepared: Option<IsolatedWorkspacePrepared>,
    pub created: Option<IsolatedWorkspaceCreated>,
    pub cleanup: Option<IsolatedWorkspaceCleanupRecorded>,
    pub binding_conflict: bool,
}

impl IsolatedWorkspaceState {
    fn new(isolated_workspace_id: WorkspaceId) -> Self {
        Self {
            isolated_workspace_id,
            prepared: None,
            created: None,
            cleanup: None,
            binding_conflict: false,
        }
    }

    #[must_use]
    pub fn is_consistent(&self) -> bool {
        !self.binding_conflict
    }

    #[must_use]
    pub fn requires_cleanup(&self) -> bool {
        let has_materialization_authority = self.prepared.is_some() || self.created.is_some();
        has_materialization_authority
            && !self
                .cleanup
                .as_ref()
                .is_some_and(|cleanup| cleanup.status.is_terminal())
    }
}

fn prepared_matches_created(
    prepared: &IsolatedWorkspacePrepared,
    created: &IsolatedWorkspaceCreated,
) -> bool {
    prepared.isolated_workspace_id == created.isolated_workspace_id
        && prepared.parent_workspace_id == created.parent_workspace_id
        && prepared.owner_agent_id == created.owner_agent_id
        && prepared.isolation_mode == created.isolation_mode
        && prepared.base_snapshot_id == created.base_snapshot_id
        && prepared.backend == created.backend
}

impl WriteLeaseState {
    fn new(lease_id: WriteLeaseId) -> Self {
        Self {
            lease_id,
            acquired: None,
            released: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.acquired.is_some() && self.released.is_none()
    }
}

/// Latest projected state for one merge review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeReviewState {
    pub review_id: MergeReviewId,
    pub requested: Option<MergeReviewRequested>,
    pub resolved: Option<MergeReviewResolved>,
}

impl MergeReviewState {
    fn new(review_id: MergeReviewId) -> Self {
        Self {
            review_id,
            requested: None,
            resolved: None,
        }
    }

    pub fn is_pending(&self) -> bool {
        self.requested.is_some() && self.resolved.is_none()
    }
}

/// Stable reference to the write-isolation record encountered during replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteIsolationRecordRef {
    WriteLease { lease_id: WriteLeaseId },
    IsolatedWorkspace { workspace_id: WorkspaceId },
    IsolatedChangeSet { changeset_id: ChangeSetId },
    MergeReview { review_id: MergeReviewId },
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        bail!("{label} cannot be empty");
    }
    if value == "." || value == ".." || value.contains('/') || value.contains('\\') {
        bail!("{label} must not contain path separators or traversal");
    }
    if !value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to canonicalize workspace {}",
            workspace_root.display()
        )
    })
}

fn changeset_merge_batch_id(review_id: &MergeReviewId, change_set_id: &ChangeSetId) -> String {
    format!(
        "merge-batch-{}",
        stable_event_uuid(
            "sigil-changeset-merge-batch",
            &format!("{}:{}", review_id.as_str(), change_set_id.as_str()),
        )
    )
}

fn changeset_merge_operation_id(review_id: &MergeReviewId, change_set_id: &ChangeSetId) -> String {
    format!(
        "merge-op-{}",
        stable_event_uuid(
            "sigil-changeset-merge-operation",
            &format!("{}:{}", review_id.as_str(), change_set_id.as_str()),
        )
    )
}

fn failed_changeset_file_operation_id(
    batch_id: &str,
    path: &str,
    action: ChangeSetFileAction,
) -> String {
    format!(
        "merge-file-{}",
        stable_event_uuid(
            "sigil-changeset-merge-file",
            &format!("{batch_id}:{path}:{}", action.as_str()),
        )
    )
}

fn changeset_touched_subjects(change_set: &ChangeSet) -> Result<Vec<MutationSubject>> {
    change_set
        .files
        .iter()
        .map(|file| {
            Ok(MutationSubject::File {
                path: changeset_relative_path(&file.path)?,
                file_type: crate::FileType::File,
            })
        })
        .collect()
}

fn apply_changeset_file_to_parent(
    recorder: &MutationEventRecorder,
    workspace_root: &Path,
    tool_call_id: &str,
    batch_id: &str,
    file: &ChangeSetFile,
    artifact: &UnifiedDiffArtifact,
) -> Result<AppliedChangeSetFile> {
    let relative_path = changeset_relative_path(&file.path)?;
    let absolute_path = workspace_root.join(&relative_path);
    validate_declared_before_hash(file, &absolute_path)?;
    match file.action {
        ChangeSetFileAction::Create => {
            if file_content_hash(&absolute_path)?.is_some() {
                bail!("changeset create target already exists: {}", file.path);
            }
            let content = artifact.materialize(&relative_path, &absolute_path, true)?;
            validate_declared_after_hash(file, Some(&content))?;
            let committed = write_file_with_mutation_in_batch(
                Some(recorder),
                workspace_root,
                tool_call_id,
                Some(batch_id.to_owned()),
                relative_path,
                absolute_path,
                &content,
            )?
            .ok_or_else(|| anyhow::anyhow!("durable recorder did not return mutation commit"))?;
            Ok(AppliedChangeSetFile {
                operation_id: committed.operation_id,
                workspace_snapshot_id: committed.workspace_snapshot_id,
            })
        }
        ChangeSetFileAction::Update => {
            let content = artifact.materialize(&relative_path, &absolute_path, false)?;
            validate_declared_after_hash(file, Some(&content))?;
            let committed = write_file_with_mutation_in_batch(
                Some(recorder),
                workspace_root,
                tool_call_id,
                Some(batch_id.to_owned()),
                relative_path,
                absolute_path,
                &content,
            )?
            .ok_or_else(|| anyhow::anyhow!("durable recorder did not return mutation commit"))?;
            Ok(AppliedChangeSetFile {
                operation_id: committed.operation_id,
                workspace_snapshot_id: committed.workspace_snapshot_id,
            })
        }
        ChangeSetFileAction::Delete => {
            validate_declared_after_hash(file, None)?;
            let committed = delete_file_with_mutation_in_batch(
                Some(recorder),
                workspace_root,
                tool_call_id,
                Some(batch_id.to_owned()),
                relative_path,
                absolute_path,
            )?
            .ok_or_else(|| anyhow::anyhow!("durable recorder did not return mutation commit"))?;
            Ok(AppliedChangeSetFile {
                operation_id: committed.operation_id,
                workspace_snapshot_id: committed.workspace_snapshot_id,
            })
        }
        ChangeSetFileAction::Rename => {
            bail!("changeset rename apply is not supported in changeset-only merge handoff")
        }
    }
}

#[derive(Debug)]
struct AppliedChangeSetFile {
    operation_id: OperationId,
    workspace_snapshot_id: WorkspaceSnapshotId,
}

fn validate_declared_before_hash(file: &ChangeSetFile, absolute_path: &Path) -> Result<()> {
    let Some(expected) = file.before_hash.as_deref() else {
        return Ok(());
    };
    let current = file_content_hash(absolute_path)?;
    if current.as_deref() != Some(expected) {
        bail!(
            "changeset before_hash mismatch for {}: expected {}, observed {}",
            file.path,
            expected,
            current.as_deref().unwrap_or("absent")
        );
    }
    Ok(())
}

fn validate_declared_after_hash(file: &ChangeSetFile, content: Option<&[u8]>) -> Result<()> {
    let Some(expected) = file.after_hash.as_deref() else {
        return Ok(());
    };
    let observed = content.map(bytes_hash);
    if observed.as_deref() != Some(expected) {
        bail!(
            "changeset after_hash mismatch for {}: expected {}, observed {}",
            file.path,
            expected,
            observed.as_deref().unwrap_or("absent")
        );
    }
    Ok(())
}

fn mutation_batch_status(
    committed_operations: &[OperationId],
    failed_operations: &[OperationId],
) -> MutationBatchStatus {
    match (
        committed_operations.is_empty(),
        failed_operations.is_empty(),
    ) {
        (false, true) => MutationBatchStatus::Applied,
        (true, false) => MutationBatchStatus::Failed,
        _ => MutationBatchStatus::PartiallyApplied,
    }
}

fn changeset_result_status(status: MutationBatchStatus) -> ChangeSetResultStatus {
    match status {
        MutationBatchStatus::Applied => ChangeSetResultStatus::Applied,
        MutationBatchStatus::PartiallyApplied | MutationBatchStatus::RollbackFailed => {
            ChangeSetResultStatus::PartiallyApplied
        }
        MutationBatchStatus::Failed | MutationBatchStatus::RolledBack => {
            ChangeSetResultStatus::Failed
        }
    }
}

fn changeset_relative_path(value: &str) -> Result<PathBuf> {
    let path = Path::new(value);
    if value.trim().is_empty() || path.is_absolute() {
        bail!("changeset file path must be relative: {value}");
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            _ => bail!("changeset file path contains unsupported component: {value}"),
        }
    }
    if normalized.as_os_str().is_empty() {
        bail!("changeset file path cannot be empty");
    }
    Ok(normalized)
}

#[derive(Debug, Clone)]
struct UnifiedDiffArtifact {
    patches: Vec<UnifiedFilePatch>,
}

impl UnifiedDiffArtifact {
    fn parse(content: &str) -> Result<Self> {
        let lines = split_preserving_newline(content);
        let mut patches = Vec::new();
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index].trim_end_matches(['\r', '\n']);
            if !line.starts_with("--- ") {
                index += 1;
                continue;
            }
            let old_path = diff_path_from_header(line, "--- ")?;
            index += 1;
            if index >= lines.len() {
                bail!("unified diff missing +++ header");
            }
            let new_line = lines[index].trim_end_matches(['\r', '\n']);
            let new_path = diff_path_from_header(new_line, "+++ ")?;
            index += 1;
            let mut hunks = Vec::new();
            while index < lines.len() {
                let hunk_line = lines[index].trim_end_matches(['\r', '\n']);
                if hunk_line.starts_with("--- ") {
                    break;
                }
                if hunk_line.starts_with("@@") {
                    let old_start = parse_hunk_old_start(hunk_line)?;
                    index += 1;
                    let mut hunk_lines = Vec::new();
                    while index < lines.len() {
                        let raw = &lines[index];
                        let marker = raw.chars().next().unwrap_or('\0');
                        if raw.starts_with("@@") || raw.starts_with("--- ") {
                            break;
                        }
                        match marker {
                            ' ' => hunk_lines.push(UnifiedHunkLine::Context(raw[1..].to_owned())),
                            '-' => hunk_lines.push(UnifiedHunkLine::Remove(raw[1..].to_owned())),
                            '+' => hunk_lines.push(UnifiedHunkLine::Add(raw[1..].to_owned())),
                            '\\' => {}
                            _ => bail!("unsupported unified diff hunk line: {raw:?}"),
                        }
                        index += 1;
                    }
                    hunks.push(UnifiedHunk {
                        old_start,
                        lines: hunk_lines,
                    });
                    continue;
                }
                index += 1;
            }
            patches.push(UnifiedFilePatch {
                old_path,
                new_path,
                hunks,
            });
        }
        if patches.is_empty() {
            bail!("changeset artifact does not contain a unified diff");
        }
        Ok(Self { patches })
    }

    fn materialize(
        &self,
        relative_path: &Path,
        absolute_path: &Path,
        creating: bool,
    ) -> Result<Vec<u8>> {
        let patch = self.patch_for(relative_path).ok_or_else(|| {
            anyhow::anyhow!(
                "changeset artifact does not include unified diff for {}",
                relative_path.display()
            )
        })?;
        let old_lines = if creating {
            Vec::new()
        } else {
            let bytes = fs::read(absolute_path)
                .with_context(|| format!("failed to read {}", absolute_path.display()))?;
            let content = String::from_utf8(bytes).with_context(|| {
                format!(
                    "changeset merge only supports utf-8 text files: {}",
                    absolute_path.display()
                )
            })?;
            split_preserving_newline(&content)
        };
        let new_lines = patch.apply(&old_lines)?;
        Ok(new_lines.concat().into_bytes())
    }

    fn patch_for(&self, relative_path: &Path) -> Option<&UnifiedFilePatch> {
        self.patches.iter().find(|patch| {
            patch.new_path.as_deref() == Some(relative_path)
                || patch.old_path.as_deref() == Some(relative_path)
        })
    }
}

#[derive(Debug, Clone)]
struct UnifiedFilePatch {
    old_path: Option<PathBuf>,
    new_path: Option<PathBuf>,
    hunks: Vec<UnifiedHunk>,
}

impl UnifiedFilePatch {
    fn apply(&self, old_lines: &[String]) -> Result<Vec<String>> {
        let mut output = Vec::new();
        let mut cursor = 0usize;
        for hunk in &self.hunks {
            let hunk_start = hunk.old_start.saturating_sub(1);
            if hunk_start < cursor || hunk_start > old_lines.len() {
                bail!("unified diff hunk does not match current file");
            }
            output.extend_from_slice(&old_lines[cursor..hunk_start]);
            cursor = hunk_start;
            for line in &hunk.lines {
                match line {
                    UnifiedHunkLine::Context(text) => {
                        require_old_line(old_lines, cursor, text)?;
                        output.push(text.clone());
                        cursor += 1;
                    }
                    UnifiedHunkLine::Remove(text) => {
                        require_old_line(old_lines, cursor, text)?;
                        cursor += 1;
                    }
                    UnifiedHunkLine::Add(text) => output.push(text.clone()),
                }
            }
        }
        output.extend_from_slice(&old_lines[cursor..]);
        Ok(output)
    }
}

#[derive(Debug, Clone)]
struct UnifiedHunk {
    old_start: usize,
    lines: Vec<UnifiedHunkLine>,
}

#[derive(Debug, Clone)]
enum UnifiedHunkLine {
    Context(String),
    Remove(String),
    Add(String),
}

fn require_old_line(old_lines: &[String], cursor: usize, expected: &str) -> Result<()> {
    let observed = old_lines
        .get(cursor)
        .ok_or_else(|| anyhow::anyhow!("unified diff hunk extends past end of file"))?;
    if observed != expected {
        bail!("unified diff hunk does not match current file");
    }
    Ok(())
}

fn split_preserving_newline(content: &str) -> Vec<String> {
    if content.is_empty() {
        return Vec::new();
    }
    content.split_inclusive('\n').map(str::to_owned).collect()
}

fn diff_path_from_header(line: &str, prefix: &str) -> Result<Option<PathBuf>> {
    let path = line
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow::anyhow!("invalid unified diff path header: {line}"))?
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing unified diff path in header: {line}"))?;
    if path == "/dev/null" {
        return Ok(None);
    }
    let path = path
        .strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path);
    changeset_relative_path(path).map(Some)
}

fn parse_hunk_old_start(line: &str) -> Result<usize> {
    let start = line
        .strip_prefix("@@ -")
        .ok_or_else(|| anyhow::anyhow!("invalid unified diff hunk header: {line}"))?;
    let end = start
        .find([' ', ','])
        .ok_or_else(|| anyhow::anyhow!("invalid unified diff hunk old range: {line}"))?;
    start[..end]
        .parse::<usize>()
        .with_context(|| format!("invalid unified diff hunk old start: {line}"))
}

#[cfg(test)]
#[path = "tests/write_isolation_tests.rs"]
mod tests;
