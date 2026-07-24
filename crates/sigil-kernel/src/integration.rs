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
    ChangeSet, ChangeSetFileAction, ChangeSetId, ReceiptStatus, TaskId, TaskStepId,
    VerificationReceipt, WorkspaceSnapshotId,
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
    /// The child did not provide enough structured evidence to classify its effect.
    #[default]
    Unknown,
    /// Ordinary isolated file changes.
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
            Self::Unknown => "unknown",
            Self::Files => "files",
            Self::GeneratedArtifacts => "generated_artifacts",
            Self::Global => "global",
        }
    }
}

/// Exact representation used to materialize every proposal in one integration batch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationBaseRepresentation {
    /// Legacy or incomplete proposal that cannot be admitted to an automatic lane.
    #[default]
    Unknown,
    /// Clean Git commit with no inherited overlay.
    CleanCommit { base_commit: String },
    /// Frozen post-overlay snapshot that cannot be represented by the base commit alone.
    SnapshotWorkspace {
        base_commit: String,
        overlay_digest: String,
    },
}

impl IntegrationBaseRepresentation {
    #[must_use]
    pub fn is_automatic_lane_eligible(&self) -> bool {
        matches!(
            self,
            Self::CleanCommit { .. } | Self::SnapshotWorkspace { .. }
        )
    }

    fn validate(&self) -> Result<()> {
        match self {
            Self::Unknown => Ok(()),
            Self::CleanCommit { base_commit } => {
                validate_git_object_id("integration base commit", base_commit)
            }
            Self::SnapshotWorkspace {
                base_commit,
                overlay_digest,
            } => {
                validate_git_object_id("integration base commit", base_commit)?;
                validate_sha256_digest("integration overlay digest", overlay_digest)
            }
        }
    }
}

/// Materialized content classification for one changed path.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationContentClass {
    Text,
    Binary,
    Special,
    #[default]
    Unknown,
}

/// One materialized per-file before/after fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationPathFact {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_path: Option<String>,
    pub action: ChangeSetFileAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(default)]
    pub content_class: IntegrationContentClass,
}

/// Observed repository-wide effect derived from structured tool and materialization evidence.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationObservedEffect {
    Package,
    Build,
    Git,
    Formatter,
    Codegen,
    UnknownShell,
    SharedGeneratedRoot,
    Unknown,
}

/// Why one proposal cannot enter an automatic parallel integration lane.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationFactGap {
    UnknownBaseRepresentation,
    MissingArtifactRef,
    MissingBeforeHash,
    MissingAfterHash,
    MissingRenameSource,
    UnknownContentClass,
    UnsupportedContentClass,
    UnknownDeclaredEffect,
    UnknownObservedEffect,
}

/// Content-bound terminal facts carried by one isolated child proposal.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationProposalFacts {
    #[serde(default)]
    pub base_representation: IntegrationBaseRepresentation,
    #[serde(default)]
    pub paths: Vec<IntegrationPathFact>,
    #[serde(default)]
    pub declared_effect: IntegrationEffect,
    #[serde(default)]
    pub observed_effects: Vec<IntegrationObservedEffect>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub changeset_artifact_ref: String,
    #[serde(default)]
    pub child_verification_refs: Vec<String>,
    #[serde(default)]
    pub gaps: Vec<IntegrationFactGap>,
}

impl IntegrationProposalFacts {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    /// Builds normalized facts from a materialized changeset and explicit runtime evidence.
    ///
    /// Missing facts are retained as typed gaps instead of being guessed safe.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths or duplicate changed-path identities.
    pub fn from_changeset(
        change_set: &ChangeSet,
        base_representation: IntegrationBaseRepresentation,
        content_class: IntegrationContentClass,
        declared_effect: IntegrationEffect,
        observed_effects: Vec<IntegrationObservedEffect>,
        changeset_artifact_ref: impl Into<String>,
        child_verification_refs: Vec<String>,
    ) -> Result<Self> {
        base_representation.validate()?;
        let mut paths = Vec::with_capacity(change_set.files.len());
        let mut identities = BTreeSet::new();
        let mut gaps = BTreeSet::new();
        if matches!(base_representation, IntegrationBaseRepresentation::Unknown) {
            gaps.insert(IntegrationFactGap::UnknownBaseRepresentation);
        }
        let changeset_artifact_ref = changeset_artifact_ref.into();
        if changeset_artifact_ref.trim().is_empty() {
            gaps.insert(IntegrationFactGap::MissingArtifactRef);
        }
        if declared_effect == IntegrationEffect::Unknown {
            gaps.insert(IntegrationFactGap::UnknownDeclaredEffect);
        }
        if observed_effects.contains(&IntegrationObservedEffect::Unknown) {
            gaps.insert(IntegrationFactGap::UnknownObservedEffect);
        }
        match content_class {
            IntegrationContentClass::Unknown => {
                gaps.insert(IntegrationFactGap::UnknownContentClass);
            }
            IntegrationContentClass::Binary | IntegrationContentClass::Special => {
                gaps.insert(IntegrationFactGap::UnsupportedContentClass);
            }
            IntegrationContentClass::Text => {}
        }
        for file in &change_set.files {
            let path = normalized_relative_path(&file.path)?;
            let previous_path = file
                .previous_path
                .as_deref()
                .map(normalized_relative_path)
                .transpose()?;
            if !identities.insert((path.clone(), previous_path.clone())) {
                bail!(
                    "integration proposal {} contains duplicate changed-path facts",
                    change_set.id.as_str()
                );
            }
            match file.action {
                ChangeSetFileAction::Create => {
                    if file.after_hash.as_deref().is_none_or(str::is_empty) {
                        gaps.insert(IntegrationFactGap::MissingAfterHash);
                    }
                }
                ChangeSetFileAction::Update => {
                    if file.before_hash.as_deref().is_none_or(str::is_empty) {
                        gaps.insert(IntegrationFactGap::MissingBeforeHash);
                    }
                    if file.after_hash.as_deref().is_none_or(str::is_empty) {
                        gaps.insert(IntegrationFactGap::MissingAfterHash);
                    }
                }
                ChangeSetFileAction::Delete => {
                    if file.before_hash.as_deref().is_none_or(str::is_empty) {
                        gaps.insert(IntegrationFactGap::MissingBeforeHash);
                    }
                }
                ChangeSetFileAction::Rename => {
                    if previous_path.is_none() {
                        gaps.insert(IntegrationFactGap::MissingRenameSource);
                    }
                    if file.before_hash.as_deref().is_none_or(str::is_empty) {
                        gaps.insert(IntegrationFactGap::MissingBeforeHash);
                    }
                    if file.after_hash.as_deref().is_none_or(str::is_empty) {
                        gaps.insert(IntegrationFactGap::MissingAfterHash);
                    }
                }
            }
            paths.push(IntegrationPathFact {
                path,
                previous_path,
                action: file.action,
                before_hash: file.before_hash.clone(),
                after_hash: file.after_hash.clone(),
                content_class,
            });
        }
        paths.sort_by(|left, right| {
            (&left.path, &left.previous_path).cmp(&(&right.path, &right.previous_path))
        });
        let observed_effects = observed_effects
            .into_iter()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let child_verification_refs = child_verification_refs
            .into_iter()
            .filter(|value| !value.trim().is_empty())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        Ok(Self {
            base_representation,
            paths,
            declared_effect,
            observed_effects,
            changeset_artifact_ref,
            child_verification_refs,
            gaps: gaps.into_iter().collect(),
        })
    }

    #[must_use]
    pub fn requires_manual_review(&self) -> bool {
        !self.gaps.is_empty()
            || !self.base_representation.is_automatic_lane_eligible()
            || self.changeset_artifact_ref.trim().is_empty()
            || self.declared_effect == IntegrationEffect::Unknown
            || self
                .observed_effects
                .contains(&IntegrationObservedEffect::Unknown)
            || self.paths.iter().any(|fact| {
                fact.content_class != IntegrationContentClass::Text
                    || match fact.action {
                        ChangeSetFileAction::Create => {
                            fact.after_hash.as_deref().is_none_or(str::is_empty)
                        }
                        ChangeSetFileAction::Update => {
                            fact.before_hash.as_deref().is_none_or(str::is_empty)
                                || fact.after_hash.as_deref().is_none_or(str::is_empty)
                        }
                        ChangeSetFileAction::Delete => {
                            fact.before_hash.as_deref().is_none_or(str::is_empty)
                        }
                        ChangeSetFileAction::Rename => {
                            fact.previous_path.is_none()
                                || fact.before_hash.as_deref().is_none_or(str::is_empty)
                                || fact.after_hash.as_deref().is_none_or(str::is_empty)
                        }
                    }
            })
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
    #[serde(default)]
    pub facts: IntegrationProposalFacts,
}

impl IntegrationProposalSpec {
    /// Revalidates durable proposal facts before graph or physical-lane admission.
    ///
    /// # Errors
    ///
    /// Returns an error when normalized paths, declared effects, or base identities disagree.
    pub fn validate(&self) -> Result<()> {
        if self.base_snapshot_id.trim().is_empty() {
            bail!("integration proposal base snapshot id must not be empty");
        }
        if self.verification_scope_hash.trim().is_empty() {
            bail!("integration proposal verification scope hash must not be empty");
        }
        self.facts.base_representation.validate()?;
        let requires_manual_review = self.facts.requires_manual_review();
        if !requires_manual_review && self.effect != self.facts.declared_effect {
            bail!("integration proposal declared effect disagrees with terminal facts");
        }
        let changed_paths = self
            .changed_paths
            .iter()
            .map(|path| normalized_relative_path(path))
            .collect::<Result<BTreeSet<_>>>()?;
        if changed_paths.len() != self.changed_paths.len() || changed_paths.is_empty() {
            bail!("integration proposal changed-path facts are empty or duplicated");
        }
        let fact_paths = self
            .facts
            .paths
            .iter()
            .flat_map(|fact| [Some(fact.path.as_str()), fact.previous_path.as_deref()])
            .flatten()
            .map(normalized_relative_path)
            .collect::<Result<BTreeSet<_>>>()?;
        if !requires_manual_review && fact_paths != changed_paths {
            bail!("integration proposal changed paths disagree with terminal path facts");
        }
        let generated_artifacts = self
            .generated_artifacts
            .iter()
            .map(|path| normalized_relative_path(path))
            .collect::<Result<BTreeSet<_>>>()?;
        if generated_artifacts.len() != self.generated_artifacts.len() {
            bail!("integration proposal generated-artifact facts are duplicated");
        }
        let has_global_effect = self.facts.observed_effects.iter().any(|effect| {
            matches!(
                effect,
                IntegrationObservedEffect::Package
                    | IntegrationObservedEffect::Build
                    | IntegrationObservedEffect::Git
                    | IntegrationObservedEffect::Formatter
                    | IntegrationObservedEffect::Codegen
                    | IntegrationObservedEffect::UnknownShell
                    | IntegrationObservedEffect::Unknown
            )
        });
        if !requires_manual_review && has_global_effect && self.effect != IntegrationEffect::Global
        {
            bail!("integration proposal global effect was not declared global");
        }
        if !requires_manual_review
            && self
                .facts
                .observed_effects
                .contains(&IntegrationObservedEffect::SharedGeneratedRoot)
            && self.effect == IntegrationEffect::Files
        {
            bail!("integration proposal generated effect was declared as ordinary files");
        }
        Ok(())
    }

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
        facts: IntegrationProposalFacts,
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
        let proposal = Self {
            change_set_id: change_set.id.clone(),
            step_id,
            base_snapshot_id,
            changed_paths: changed_paths.into_iter().collect(),
            depends_on: depends_on.into_iter().collect(),
            generated_artifacts,
            effect,
            verification_scope_hash,
            facts,
        };
        proposal.validate()?;
        Ok(proposal)
    }
}

/// Why two proposals must serialize in one conflict component.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationConflictReason {
    BaseSnapshotMismatch,
    BaseRepresentationMismatch,
    ChangedPathOverlap,
    TaskDependency,
    GeneratedArtifactOverlap,
    VerificationScopeMismatch,
    PackageEffect,
    BuildEffect,
    GitEffect,
    GlobalEffect,
    IncompleteEffectFacts,
}

impl IntegrationConflictReason {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BaseSnapshotMismatch => "base_snapshot_mismatch",
            Self::BaseRepresentationMismatch => "base_representation_mismatch",
            Self::ChangedPathOverlap => "changed_path_overlap",
            Self::TaskDependency => "task_dependency",
            Self::GeneratedArtifactOverlap => "generated_artifact_overlap",
            Self::VerificationScopeMismatch => "verification_scope_mismatch",
            Self::PackageEffect => "package_effect",
            Self::BuildEffect => "build_effect",
            Self::GitEffect => "git_effect",
            Self::GlobalEffect => "global_effect",
            Self::IncompleteEffectFacts => "incomplete_effect_facts",
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
    pub base_representation: IntegrationBaseRepresentation,
    pub proposals: Vec<IntegrationProposalSpec>,
    pub conflicts: Vec<IntegrationConflictEdge>,
    pub lanes: Vec<IntegrationLaneSpec>,
}

impl IntegrationPlan {
    #[must_use]
    pub fn requires_manual_review(&self) -> bool {
        self.proposals.iter().any(|proposal| {
            proposal.facts.requires_manual_review()
                || proposal.base_snapshot_id != self.base_snapshot_id
                || proposal.facts.base_representation != self.base_representation
        })
    }
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
        proposal.validate()?;
        if proposals_by_id
            .insert(proposal.change_set_id.clone(), proposal)
            .is_some()
        {
            bail!("integration plan contains a duplicate changeset id");
        }
    }
    let ordered = proposals_by_id.into_values().collect::<Vec<_>>();
    let base_snapshot_id = ordered[0].base_snapshot_id.clone();
    let base_representation = ordered[0].facts.base_representation.clone();
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
        base_representation,
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

/// Runtime-owned physical target prepared for one integration lane.
///
/// Callers outside the runtime may display the target kind, but must not accept path/ref values
/// from a model or planner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationLaneTarget {
    ManagedRef {
        base_commit: String,
        expected_oid: String,
        private_ref: String,
    },
    SnapshotWorkspace {
        base_snapshot_id: WorkspaceSnapshotId,
        overlay_digest: String,
        revision: u64,
        owned_workspace_id: String,
    },
}

/// Recovery-critical fact recorded after a lane target is materialized and before member apply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLanePrepared {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub target: IntegrationLaneTarget,
    pub owned_workspace_id: String,
    pub ordered_members: Vec<ChangeSetId>,
    pub prepared_at_unix_ms: u64,
}

/// Exact target transition produced by one ordered lane member.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IntegrationLaneMemberEffect {
    ManagedRefAdvanced {
        expected_old_oid: String,
        new_oid: String,
        candidate_snapshot_id: WorkspaceSnapshotId,
    },
    SnapshotWorkspaceApplied {
        expected_snapshot_id: WorkspaceSnapshotId,
        expected_revision: u64,
        candidate_snapshot_id: WorkspaceSnapshotId,
        candidate_revision: u64,
    },
}

/// Recovery-critical ordered member-apply receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLaneMemberApplied {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub change_set_id: ChangeSetId,
    pub member_index: u32,
    pub effect: IntegrationLaneMemberEffect,
    pub applied_at_unix_ms: u64,
}

/// Recovery-critical link between scoped verification and an exact lane candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLaneVerificationLinked {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub candidate: IntegrationLaneCandidate,
    pub verification_check_ids: Vec<String>,
    pub verification_scope_hashes: Vec<String>,
    pub verification_receipts: Vec<VerificationReceipt>,
    pub linked_at_unix_ms: u64,
}

/// Recovery-critical terminal outcome for one physical lane attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLaneTerminal {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub status: IntegrationLaneStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<IntegrationLaneCandidate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub terminal_at_unix_ms: u64,
}

/// Cleanup or retention disposition for one runtime-owned integration workspace.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationLaneCleanupStatus {
    Retained,
    Removed,
    AlreadyMissing,
    Failed,
}

impl IntegrationLaneCleanupStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Retained => "retained",
            Self::Removed => "removed",
            Self::AlreadyMissing => "already_missing",
            Self::Failed => "failed",
        }
    }
}

/// Recovery-critical cleanup inventory for one lane-owned workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationLaneCleanupRecorded {
    pub plan_id: IntegrationPlanId,
    pub lane_id: IntegrationLaneId,
    pub owned_workspace_id: String,
    pub status: IntegrationLaneCleanupStatus,
    pub recorded_at_unix_ms: u64,
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
    pub lifecycle_lanes: BTreeMap<IntegrationLaneId, IntegrationLaneLifecycleState>,
    pub promotions: Vec<IntegrationPromotionRecorded>,
    pub inconsistent: bool,
}

/// Replayed recovery-critical state for one integration lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationLaneLifecycleState {
    pub prepared: Option<IntegrationLanePrepared>,
    pub applied_members: BTreeMap<ChangeSetId, IntegrationLaneMemberApplied>,
    pub verification: Option<IntegrationLaneVerificationLinked>,
    pub terminal: Option<IntegrationLaneTerminal>,
    pub cleanup: Option<IntegrationLaneCleanupRecorded>,
    pub inconsistent: bool,
}

impl IntegrationLaneLifecycleState {
    fn new() -> Self {
        Self {
            prepared: None,
            applied_members: BTreeMap::new(),
            verification: None,
            terminal: None,
            cleanup: None,
            inconsistent: false,
        }
    }
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
                                lifecycle_lanes: BTreeMap::new(),
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
            ControlEntry::IntegrationLanePrepared(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let Some(spec) = state
                    .recorded
                    .plan
                    .lanes
                    .iter()
                    .find(|lane| lane.lane_id == entry.lane_id)
                else {
                    state.inconsistent = true;
                    return;
                };
                let lifecycle = state
                    .lifecycle_lanes
                    .entry(entry.lane_id.clone())
                    .or_insert_with(IntegrationLaneLifecycleState::new);
                if entry.ordered_members != spec.proposals
                    || !lane_target_matches_plan(&state.recorded.plan, &entry.target)
                    || entry.owned_workspace_id.trim().is_empty()
                    || matches!(
                        &entry.target,
                        IntegrationLaneTarget::SnapshotWorkspace {
                            owned_workspace_id,
                            ..
                        } if owned_workspace_id != &entry.owned_workspace_id
                    )
                    || entry.prepared_at_unix_ms == 0
                {
                    lifecycle.inconsistent = true;
                    state.inconsistent = true;
                }
                match &lifecycle.prepared {
                    Some(existing) if existing != entry => {
                        lifecycle.inconsistent = true;
                        state.inconsistent = true;
                    }
                    Some(_) => {}
                    None => lifecycle.prepared = Some(entry.clone()),
                }
            }
            ControlEntry::IntegrationLaneMemberApplied(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let lifecycle = state
                    .lifecycle_lanes
                    .entry(entry.lane_id.clone())
                    .or_insert_with(IntegrationLaneLifecycleState::new);
                if let Some(existing) = lifecycle.applied_members.get(&entry.change_set_id) {
                    if existing != entry {
                        lifecycle.inconsistent = true;
                        state.inconsistent = true;
                    }
                    return;
                }
                let valid = lifecycle
                    .prepared
                    .as_ref()
                    .is_some_and(|prepared| member_apply_matches(prepared, lifecycle, entry));
                if !valid {
                    lifecycle.inconsistent = true;
                    state.inconsistent = true;
                }
                lifecycle
                    .applied_members
                    .insert(entry.change_set_id.clone(), entry.clone());
            }
            ControlEntry::IntegrationLaneVerificationLinked(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let Some(spec) = state
                    .recorded
                    .plan
                    .lanes
                    .iter()
                    .find(|lane| lane.lane_id == entry.lane_id)
                else {
                    state.inconsistent = true;
                    return;
                };
                let lifecycle = state
                    .lifecycle_lanes
                    .entry(entry.lane_id.clone())
                    .or_insert_with(IntegrationLaneLifecycleState::new);
                let candidate = latest_lifecycle_candidate(lifecycle);
                let receipt_check_ids = entry
                    .verification_receipts
                    .iter()
                    .map(|receipt| receipt.check_spec_id.as_str())
                    .collect::<BTreeSet<_>>();
                let receipt_scope_hashes = entry
                    .verification_receipts
                    .iter()
                    .map(|receipt| receipt.binding.verification_scope_hash.as_str())
                    .collect::<BTreeSet<_>>();
                let expected_check_ids = entry
                    .verification_check_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                let expected_scope_hashes = entry
                    .verification_scope_hashes
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>();
                if entry.verification_check_ids.is_empty()
                    || entry.verification_scope_hashes != spec.verification_scope_hashes
                    || entry.verification_receipts.is_empty()
                    || receipt_check_ids != expected_check_ids
                    || receipt_scope_hashes != expected_scope_hashes
                    || entry.verification_receipts.iter().any(|receipt| {
                        receipt.check_status != ReceiptStatus::Succeeded
                            || receipt.receipt.status != ReceiptStatus::Succeeded
                            || receipt.mutates_verification_scope
                            || receipt.binding.execution_backend.is_none()
                    })
                    || candidate.as_ref() != Some(&entry.candidate)
                    || entry.linked_at_unix_ms == 0
                {
                    lifecycle.inconsistent = true;
                    state.inconsistent = true;
                }
                match &lifecycle.verification {
                    Some(existing) if existing != entry => {
                        lifecycle.inconsistent = true;
                        state.inconsistent = true;
                    }
                    Some(_) => {}
                    None => lifecycle.verification = Some(entry.clone()),
                }
            }
            ControlEntry::IntegrationLaneTerminal(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let lifecycle = state
                    .lifecycle_lanes
                    .entry(entry.lane_id.clone())
                    .or_insert_with(IntegrationLaneLifecycleState::new);
                let ready_matches = entry.status != IntegrationLaneStatus::Ready
                    || lifecycle.verification.as_ref().is_some_and(|verification| {
                        entry.candidate.as_ref() == Some(&verification.candidate)
                    });
                if lifecycle.prepared.is_none()
                    || !entry.status.is_terminal()
                    || !ready_matches
                    || entry.terminal_at_unix_ms == 0
                {
                    lifecycle.inconsistent = true;
                    state.inconsistent = true;
                }
                match &lifecycle.terminal {
                    Some(existing) if existing != entry => {
                        lifecycle.inconsistent = true;
                        state.inconsistent = true;
                    }
                    Some(_) => {}
                    None => lifecycle.terminal = Some(entry.clone()),
                }
            }
            ControlEntry::IntegrationLaneCleanupRecorded(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let lifecycle = state
                    .lifecycle_lanes
                    .entry(entry.lane_id.clone())
                    .or_insert_with(IntegrationLaneLifecycleState::new);
                let valid = lifecycle.prepared.as_ref().is_some_and(|prepared| {
                    prepared.owned_workspace_id == entry.owned_workspace_id
                }) && entry.recorded_at_unix_ms > 0;
                if !valid {
                    lifecycle.inconsistent = true;
                    state.inconsistent = true;
                }
                match &lifecycle.cleanup {
                    Some(existing) if existing == entry => {}
                    Some(existing)
                        if existing.status == IntegrationLaneCleanupStatus::Retained
                            && matches!(
                                entry.status,
                                IntegrationLaneCleanupStatus::Removed
                                    | IntegrationLaneCleanupStatus::AlreadyMissing
                                    | IntegrationLaneCleanupStatus::Failed
                            ) =>
                    {
                        lifecycle.cleanup = Some(entry.clone());
                    }
                    Some(_) => {
                        lifecycle.inconsistent = true;
                        state.inconsistent = true;
                    }
                    None => lifecycle.cleanup = Some(entry.clone()),
                }
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

fn lane_target_matches_plan(plan: &IntegrationPlan, target: &IntegrationLaneTarget) -> bool {
    match (&plan.base_representation, target) {
        (
            IntegrationBaseRepresentation::CleanCommit { base_commit },
            IntegrationLaneTarget::ManagedRef {
                base_commit: target_base_commit,
                expected_oid,
                private_ref,
            },
        ) => {
            base_commit == target_base_commit
                && validate_git_object_id("integration lane expected oid", expected_oid).is_ok()
                && !private_ref.trim().is_empty()
        }
        (
            IntegrationBaseRepresentation::SnapshotWorkspace { overlay_digest, .. },
            IntegrationLaneTarget::SnapshotWorkspace {
                base_snapshot_id,
                overlay_digest: target_overlay,
                revision,
                owned_workspace_id,
            },
        ) => {
            !base_snapshot_id.trim().is_empty()
                && overlay_digest == target_overlay
                && *revision == 0
                && !owned_workspace_id.trim().is_empty()
        }
        _ => false,
    }
}

fn member_apply_matches(
    prepared: &IntegrationLanePrepared,
    lifecycle: &IntegrationLaneLifecycleState,
    entry: &IntegrationLaneMemberApplied,
) -> bool {
    let Ok(member_index) = usize::try_from(entry.member_index) else {
        return false;
    };
    if prepared.ordered_members.get(member_index) != Some(&entry.change_set_id)
        || lifecycle.applied_members.len() != member_index
        || entry.applied_at_unix_ms == 0
    {
        return false;
    }
    let previous = member_index.checked_sub(1).and_then(|index| {
        prepared
            .ordered_members
            .get(index)
            .and_then(|id| lifecycle.applied_members.get(id))
    });
    match (&prepared.target, previous, &entry.effect) {
        (
            IntegrationLaneTarget::ManagedRef { expected_oid, .. },
            None,
            IntegrationLaneMemberEffect::ManagedRefAdvanced {
                expected_old_oid, ..
            },
        ) => expected_oid == expected_old_oid,
        (
            IntegrationLaneTarget::ManagedRef { .. },
            Some(previous),
            IntegrationLaneMemberEffect::ManagedRefAdvanced {
                expected_old_oid, ..
            },
        ) => matches!(
            &previous.effect,
            IntegrationLaneMemberEffect::ManagedRefAdvanced { new_oid, .. }
                if new_oid == expected_old_oid
        ),
        (
            IntegrationLaneTarget::SnapshotWorkspace {
                base_snapshot_id,
                revision,
                ..
            },
            None,
            IntegrationLaneMemberEffect::SnapshotWorkspaceApplied {
                expected_snapshot_id,
                expected_revision,
                candidate_revision,
                ..
            },
        ) => {
            base_snapshot_id == expected_snapshot_id
                && revision == expected_revision
                && *candidate_revision == expected_revision.saturating_add(1)
        }
        (
            IntegrationLaneTarget::SnapshotWorkspace { .. },
            Some(previous),
            IntegrationLaneMemberEffect::SnapshotWorkspaceApplied {
                expected_snapshot_id,
                expected_revision,
                candidate_revision,
                ..
            },
        ) => matches!(
            &previous.effect,
            IntegrationLaneMemberEffect::SnapshotWorkspaceApplied {
                candidate_snapshot_id,
                candidate_revision: previous_revision,
                ..
            } if candidate_snapshot_id == expected_snapshot_id
                && previous_revision == expected_revision
                && *candidate_revision == expected_revision.saturating_add(1)
        ),
        _ => false,
    }
}

fn latest_lifecycle_candidate(
    lifecycle: &IntegrationLaneLifecycleState,
) -> Option<IntegrationLaneCandidate> {
    let prepared = lifecycle.prepared.as_ref()?;
    let last_id = prepared.ordered_members.last()?;
    let applied = lifecycle.applied_members.get(last_id)?;
    match (&prepared.target, &applied.effect) {
        (
            IntegrationLaneTarget::ManagedRef {
                base_commit,
                expected_oid: _,
                private_ref,
            },
            IntegrationLaneMemberEffect::ManagedRefAdvanced {
                new_oid,
                candidate_snapshot_id,
                ..
            },
        ) => Some(IntegrationLaneCandidate::ManagedRef {
            private_ref: private_ref.clone(),
            base_commit: base_commit.clone(),
            candidate_commit: new_oid.clone(),
            workspace_snapshot_id: candidate_snapshot_id.clone(),
        }),
        (
            IntegrationLaneTarget::SnapshotWorkspace {
                base_snapshot_id,
                overlay_digest,
                owned_workspace_id,
                ..
            },
            IntegrationLaneMemberEffect::SnapshotWorkspaceApplied {
                candidate_snapshot_id,
                candidate_revision,
                ..
            },
        ) => Some(IntegrationLaneCandidate::SnapshotWorkspace {
            owned_workspace_id: owned_workspace_id.clone(),
            base_snapshot_id: base_snapshot_id.clone(),
            overlay_digest: overlay_digest.clone(),
            revision: *candidate_revision,
            candidate_snapshot_id: candidate_snapshot_id.clone(),
        }),
        _ => None,
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
    if left.facts.base_representation != right.facts.base_representation {
        reasons.insert(IntegrationConflictReason::BaseRepresentationMismatch);
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
    if left.verification_scope_hash != right.verification_scope_hash {
        reasons.insert(IntegrationConflictReason::VerificationScopeMismatch);
    }
    let observed_effects = left
        .facts
        .observed_effects
        .iter()
        .chain(&right.facts.observed_effects)
        .copied()
        .collect::<BTreeSet<_>>();
    if observed_effects.contains(&IntegrationObservedEffect::Package) {
        reasons.insert(IntegrationConflictReason::PackageEffect);
    }
    if observed_effects.contains(&IntegrationObservedEffect::Build) {
        reasons.insert(IntegrationConflictReason::BuildEffect);
    }
    if observed_effects.contains(&IntegrationObservedEffect::Git) {
        reasons.insert(IntegrationConflictReason::GitEffect);
    }
    if left.effect == IntegrationEffect::Global
        || right.effect == IntegrationEffect::Global
        || observed_effects.iter().any(|effect| {
            matches!(
                effect,
                IntegrationObservedEffect::Formatter
                    | IntegrationObservedEffect::Codegen
                    | IntegrationObservedEffect::UnknownShell
                    | IntegrationObservedEffect::Unknown
            )
        })
    {
        reasons.insert(IntegrationConflictReason::GlobalEffect);
    }
    if left.facts.requires_manual_review() || right.facts.requires_manual_review() {
        reasons.insert(IntegrationConflictReason::IncompleteEffectFacts);
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

fn validate_git_object_id(label: &str, value: &str) -> Result<()> {
    if !(40..=64).contains(&value.len()) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must be a 40 to 64 character hexadecimal object id");
    }
    Ok(())
}

fn validate_sha256_digest(label: &str, value: &str) -> Result<()> {
    let Some(value) = value.strip_prefix("sha256:") else {
        bail!("{label} must use the sha256 prefix");
    };
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must contain exactly 64 hexadecimal characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/integration_tests.rs"]
mod tests;
