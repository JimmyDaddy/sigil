//! Durable integration-lane planning and promotion facts.
//!
//! The kernel owns the provider-neutral conflict graph and append-only projection. Physical Git
//! worktrees, private integration refs, and command execution remain runtime responsibilities.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::{Component, Path},
};

use anyhow::{Result, bail};
use serde::{Deserialize, Deserializer, Serialize};

use crate::{
    ChangeSet, ChangeSetId, TaskId, TaskStepId, WorkspaceSnapshotId,
    session::{ControlEntry, SessionLogEntry},
};

/// Stable identifier for one deterministic integration plan.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct IntegrationPlanId(String);

impl IntegrationPlanId {
    /// Creates a path-safe integration plan identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty or unsafe for a private ref/path component.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("integration plan id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IntegrationPlanId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Stable identifier for one integration lane inside a plan.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct IntegrationLaneId(String);

impl IntegrationLaneId {
    /// Creates a path-safe integration lane identifier.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is empty or unsafe for a private ref/path component.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("integration lane id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IntegrationLaneId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Effect domain used to decide whether otherwise disjoint proposals may integrate independently.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationEffect {
    /// Ordinary isolated file changes.
    #[default]
    Files,
    /// A proposal touches one or more declared generated artifacts.
    GeneratedArtifacts,
    /// A package, repository, formatter, codegen, or other global effect requires serialization.
    Global,
}

impl IntegrationEffect {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::GeneratedArtifacts => "generated_artifacts",
            Self::Global => "global",
        }
    }
}

/// One proposal admitted to conflict-graph planning.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationProposalSpec {
    pub change_set_id: ChangeSetId,
    pub step_id: TaskStepId,
    pub base_snapshot_id: WorkspaceSnapshotId,
    #[serde(default)]
    pub changed_paths: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<ChangeSetId>,
    #[serde(default)]
    pub generated_artifacts: Vec<String>,
    #[serde(default)]
    pub effect: IntegrationEffect,
    pub verification_scope_hash: String,
}

impl IntegrationProposalSpec {
    /// Builds a normalized proposal from a changeset and task-DAG facts.
    ///
    /// # Errors
    ///
    /// Returns an error for empty/incomplete identity, unsafe paths, or duplicate path facts.
    pub fn from_changeset(
        change_set: &ChangeSet,
        step_id: TaskStepId,
        base_snapshot_id: WorkspaceSnapshotId,
        depends_on: Vec<ChangeSetId>,
        generated_artifacts: Vec<String>,
        effect: IntegrationEffect,
        verification_scope_hash: impl Into<String>,
    ) -> Result<Self> {
        if base_snapshot_id.trim().is_empty() {
            bail!("integration proposal base snapshot id must not be empty");
        }
        let verification_scope_hash = verification_scope_hash.into();
        if verification_scope_hash.trim().is_empty() {
            bail!("integration proposal verification scope hash must not be empty");
        }
        let mut changed_paths = BTreeSet::new();
        for file in &change_set.files {
            changed_paths.insert(normalized_relative_path(&file.path)?);
            if let Some(previous_path) = &file.previous_path {
                changed_paths.insert(normalized_relative_path(previous_path)?);
            }
        }
        if changed_paths.is_empty() {
            bail!(
                "integration proposal {} has no changed paths",
                change_set.id.as_str()
            );
        }
        let generated_artifacts = generated_artifacts
            .into_iter()
            .map(|path| normalized_relative_path(&path))
            .collect::<Result<BTreeSet<_>>>()?
            .into_iter()
            .collect();
        let depends_on = depends_on.into_iter().collect::<BTreeSet<_>>();
        if depends_on.contains(&change_set.id) {
            bail!(
                "integration proposal {} cannot depend on itself",
                change_set.id.as_str()
            );
        }
        Ok(Self {
            change_set_id: change_set.id.clone(),
            step_id,
            base_snapshot_id,
            changed_paths: changed_paths.into_iter().collect(),
            depends_on: depends_on.into_iter().collect(),
            generated_artifacts,
            effect,
            verification_scope_hash,
        })
    }
}

/// Why two proposals must serialize in one conflict component.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationConflictReason {
    BaseSnapshotMismatch,
    ChangedPathOverlap,
    TaskDependency,
    GeneratedArtifactOverlap,
    GlobalEffect,
}

impl IntegrationConflictReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaseSnapshotMismatch => "base_snapshot_mismatch",
            Self::ChangedPathOverlap => "changed_path_overlap",
            Self::TaskDependency => "task_dependency",
            Self::GeneratedArtifactOverlap => "generated_artifact_overlap",
            Self::GlobalEffect => "global_effect",
        }
    }
}

/// One undirected conflict-graph edge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationConflictEdge {
    pub left: ChangeSetId,
    pub right: ChangeSetId,
    pub reasons: Vec<IntegrationConflictReason>,
}

/// Ordered proposals that must be integrated serially inside one lane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLaneSpec {
    pub lane_id: IntegrationLaneId,
    pub proposals: Vec<ChangeSetId>,
    #[serde(default)]
    pub verification_scope_hashes: Vec<String>,
}

/// Deterministic conflict graph and lane assignment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationPlan {
    pub plan_id: IntegrationPlanId,
    pub task_id: TaskId,
    pub plan_version: u32,
    pub base_snapshot_id: WorkspaceSnapshotId,
    pub proposals: Vec<IntegrationProposalSpec>,
    pub conflicts: Vec<IntegrationConflictEdge>,
    pub lanes: Vec<IntegrationLaneSpec>,
}

/// Builds deterministic conflict components for a same-task proposal batch.
///
/// Every connected conflict component becomes one serial lane. Isolated vertices become distinct
/// lanes and may integrate concurrently. A mixed-base batch is retained as a conflict graph rather
/// than silently rebased.
///
/// # Errors
///
/// Returns an error for an empty batch, duplicate changeset ids, or an empty plan identity.
pub fn build_integration_plan(
    plan_id: IntegrationPlanId,
    task_id: TaskId,
    plan_version: u32,
    proposals: Vec<IntegrationProposalSpec>,
) -> Result<IntegrationPlan> {
    if proposals.is_empty() {
        bail!("integration plan requires at least one proposal");
    }
    let mut proposals_by_id = BTreeMap::new();
    for proposal in proposals {
        if proposals_by_id
            .insert(proposal.change_set_id.clone(), proposal)
            .is_some()
        {
            bail!("integration plan contains a duplicate changeset id");
        }
    }
    let ordered = proposals_by_id.into_values().collect::<Vec<_>>();
    let base_snapshot_id = ordered[0].base_snapshot_id.clone();
    let mut conflicts = Vec::new();
    let mut adjacency = BTreeMap::<ChangeSetId, BTreeSet<ChangeSetId>>::new();
    for proposal in &ordered {
        adjacency.entry(proposal.change_set_id.clone()).or_default();
    }
    for left_index in 0..ordered.len() {
        for right_index in (left_index + 1)..ordered.len() {
            let left = &ordered[left_index];
            let right = &ordered[right_index];
            let reasons = conflict_reasons(left, right);
            if reasons.is_empty() {
                continue;
            }
            adjacency
                .entry(left.change_set_id.clone())
                .or_default()
                .insert(right.change_set_id.clone());
            adjacency
                .entry(right.change_set_id.clone())
                .or_default()
                .insert(left.change_set_id.clone());
            conflicts.push(IntegrationConflictEdge {
                left: left.change_set_id.clone(),
                right: right.change_set_id.clone(),
                reasons,
            });
        }
    }

    let proposal_lookup = ordered
        .iter()
        .map(|proposal| (proposal.change_set_id.clone(), proposal))
        .collect::<BTreeMap<_, _>>();
    let mut unvisited = ordered
        .iter()
        .map(|proposal| proposal.change_set_id.clone())
        .collect::<BTreeSet<_>>();
    let mut lanes = Vec::new();
    while let Some(first) = unvisited.iter().next().cloned() {
        let mut queue = VecDeque::from([first.clone()]);
        let mut component = BTreeSet::new();
        unvisited.remove(&first);
        while let Some(current) = queue.pop_front() {
            component.insert(current.clone());
            if let Some(neighbors) = adjacency.get(&current) {
                for neighbor in neighbors {
                    if unvisited.remove(neighbor) {
                        queue.push_back(neighbor.clone());
                    }
                }
            }
        }
        let proposals = dependency_stable_order(&component, &proposal_lookup);
        let verification_scope_hashes = proposals
            .iter()
            .filter_map(|id| proposal_lookup.get(id))
            .map(|proposal| proposal.verification_scope_hash.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let lane_id = IntegrationLaneId::new(format!("lane-{}", lanes.len() + 1))?;
        lanes.push(IntegrationLaneSpec {
            lane_id,
            proposals,
            verification_scope_hashes,
        });
    }

    Ok(IntegrationPlan {
        plan_id,
        task_id,
        plan_version,
        base_snapshot_id,
        proposals: ordered,
        conflicts,
        lanes,
    })
}

/// Runtime lifecycle for one private integration lane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationLaneStatus {
    Pending,
    Integrating,
    Verifying,
    Ready,
    Conflict,
    Stale,
    Failed,
    Promoted,
    Cancelled,
}

impl IntegrationLaneStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Integrating => "integrating",
            Self::Verifying => "verifying",
            Self::Ready => "ready",
            Self::Conflict => "conflict",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::Promoted => "promoted",
            Self::Cancelled => "cancelled",
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Ready
                | Self::Conflict
                | Self::Stale
                | Self::Failed
                | Self::Promoted
                | Self::Cancelled
        )
    }
}

/// Append-only integration plan fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationPlanRecorded {
    pub plan: IntegrationPlan,
}

/// Append-only lane transition with bounded verification/ref evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLaneChanged {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub status: IntegrationLaneStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<IntegrationLaneCandidate>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_check_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Exact runtime-owned candidate target produced by one integration lane.
///
/// Managed refs are valid only for clean commit bases. Snapshot workspaces preserve an inherited
/// dirty/untracked overlay and therefore cannot be represented as a Git ref without losing bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationLaneCandidate {
    ManagedRef {
        private_ref: String,
        base_commit: String,
        candidate_commit: String,
        workspace_snapshot_id: WorkspaceSnapshotId,
    },
    SnapshotWorkspace {
        owned_workspace_id: String,
        base_snapshot_id: WorkspaceSnapshotId,
        overlay_digest: String,
        revision: u64,
        candidate_snapshot_id: WorkspaceSnapshotId,
    },
}

/// Result of the final exact parent snapshot/ref promotion barrier.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationPromotionStatus {
    Prepared,
    Promoted,
    Conflict,
    Stale,
    Failed,
    Cancelled,
}

impl IntegrationPromotionStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Promoted => "promoted",
            Self::Conflict => "conflict",
            Self::Stale => "stale",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Mutually exclusive final promotion target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationPromotionTarget {
    WorkspaceApply {
        expected_snapshot_id: WorkspaceSnapshotId,
        expected_revision: u64,
    },
    GitRefAdvance {
        target_ref: String,
        expected_old_oid: String,
        candidate_oid: String,
    },
}

/// Exact effect observed for a successful promotion target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationPromotionEffect {
    WorkspaceApplied {
        promoted_snapshot_id: WorkspaceSnapshotId,
        promoted_revision: u64,
    },
    GitRefAdvanced {
        old_oid: String,
        new_oid: String,
    },
}

/// Append-only final promotion fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationPromotionRecorded {
    pub plan_id: IntegrationPlanId,
    pub status: IntegrationPromotionStatus,
    pub preview_digest: String,
    pub target: IntegrationPromotionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<IntegrationPromotionEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Latest replayed state for one integration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPlanState {
    pub recorded: IntegrationPlanRecorded,
    pub lanes: BTreeMap<IntegrationLaneId, IntegrationLaneChanged>,
    pub promotions: Vec<IntegrationPromotionRecorded>,
    pub inconsistent: bool,
}

/// Reconstructed integration state from append-only control entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntegrationProjection {
    pub plans: BTreeMap<IntegrationPlanId, IntegrationPlanState>,
    pub latest_plan_id: Option<IntegrationPlanId>,
}

impl IntegrationProjection {
    #[must_use]
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
            ControlEntry::IntegrationPlanRecorded(entry) => {
                self.latest_plan_id = Some(entry.plan.plan_id.clone());
                match self.plans.get_mut(&entry.plan.plan_id) {
                    Some(state) if state.recorded != *entry => state.inconsistent = true,
                    Some(_) => {}
                    None => {
                        self.plans.insert(
                            entry.plan.plan_id.clone(),
                            IntegrationPlanState {
                                recorded: entry.clone(),
                                lanes: BTreeMap::new(),
                                promotions: Vec::new(),
                                inconsistent: false,
                            },
                        );
                    }
                }
            }
            ControlEntry::IntegrationLaneChanged(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                if !state
                    .recorded
                    .plan
                    .lanes
                    .iter()
                    .any(|lane| lane.lane_id == entry.lane_id)
                {
                    state.inconsistent = true;
                    return;
                }
                state.lanes.insert(entry.lane_id.clone(), entry.clone());
            }
            ControlEntry::IntegrationPromotionRecorded(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                if entry.preview_digest.trim().is_empty() || !promotion_effect_matches_target(entry)
                {
                    state.inconsistent = true;
                }
                state.promotions.push(entry.clone());
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn latest(&self) -> Option<&IntegrationPlanState> {
        self.latest_plan_id
            .as_ref()
            .and_then(|plan_id| self.plans.get(plan_id))
    }
}

fn promotion_effect_matches_target(entry: &IntegrationPromotionRecorded) -> bool {
    match (&entry.target, &entry.effect, entry.status) {
        (
            IntegrationPromotionTarget::WorkspaceApply { .. },
            Some(IntegrationPromotionEffect::WorkspaceApplied { .. }),
            IntegrationPromotionStatus::Promoted,
        )
        | (
            IntegrationPromotionTarget::GitRefAdvance { .. },
            Some(IntegrationPromotionEffect::GitRefAdvanced { .. }),
            IntegrationPromotionStatus::Promoted,
        ) => true,
        (_, None, status) if status != IntegrationPromotionStatus::Promoted => true,
        _ => false,
    }
}

fn conflict_reasons(
    left: &IntegrationProposalSpec,
    right: &IntegrationProposalSpec,
) -> Vec<IntegrationConflictReason> {
    let mut reasons = BTreeSet::new();
    if left.base_snapshot_id != right.base_snapshot_id {
        reasons.insert(IntegrationConflictReason::BaseSnapshotMismatch);
    }
    if sets_overlap(&left.changed_paths, &right.changed_paths) {
        reasons.insert(IntegrationConflictReason::ChangedPathOverlap);
    }
    if left.depends_on.contains(&right.change_set_id)
        || right.depends_on.contains(&left.change_set_id)
    {
        reasons.insert(IntegrationConflictReason::TaskDependency);
    }
    if sets_overlap(&left.generated_artifacts, &right.generated_artifacts) {
        reasons.insert(IntegrationConflictReason::GeneratedArtifactOverlap);
    }
    if left.effect == IntegrationEffect::Global || right.effect == IntegrationEffect::Global {
        reasons.insert(IntegrationConflictReason::GlobalEffect);
    }
    reasons.into_iter().collect()
}

fn sets_overlap(left: &[String], right: &[String]) -> bool {
    let left = left.iter().collect::<BTreeSet<_>>();
    right.iter().any(|value| left.contains(value))
}

fn dependency_stable_order(
    component: &BTreeSet<ChangeSetId>,
    proposals: &BTreeMap<ChangeSetId, &IntegrationProposalSpec>,
) -> Vec<ChangeSetId> {
    let mut remaining = component.clone();
    let mut ordered = Vec::with_capacity(component.len());
    while !remaining.is_empty() {
        let next = remaining.iter().find(|candidate| {
            proposals.get(*candidate).is_none_or(|proposal| {
                proposal
                    .depends_on
                    .iter()
                    .filter(|dependency| component.contains(*dependency))
                    .all(|dependency| ordered.contains(dependency))
            })
        });
        let next = next.cloned().unwrap_or_else(|| {
            remaining
                .iter()
                .next()
                .expect("remaining component is non-empty")
                .clone()
        });
        remaining.remove(&next);
        ordered.push(next);
    }
    ordered
}

fn normalized_relative_path(value: &str) -> Result<String> {
    if value.trim().is_empty() {
        bail!("integration path must not be empty");
    }
    let path = Path::new(value);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("integration path must be a normalized relative path: {value}");
    }
    let normalized = path.to_string_lossy().replace('\\', "/");
    if normalized.len() > 4096 {
        bail!("integration path exceeds the 4096 byte limit");
    }
    Ok(normalized)
}

fn validate_stable_id(label: &str, value: &str) -> Result<()> {
    if value.is_empty() || value == "." || value == ".." {
        bail!("{label} must not be empty or traversal-like");
    }
    if value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        bail!("{label} contains unsupported characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/integration_tests.rs"]
mod tests;
