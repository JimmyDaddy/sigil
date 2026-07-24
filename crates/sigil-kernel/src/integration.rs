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
    VerificationReceipt, VerificationVerdict, WorkspaceSnapshotId,
    session::{ControlEntry, SessionLogEntry},
    sha256_hex,
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

/// Stable identity for one physical promotion attempt.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct IntegrationPromotionAttemptId(String);

impl IntegrationPromotionAttemptId {
    /// Creates one path-safe promotion attempt identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identity is empty or contains unstable characters.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_stable_id("integration promotion attempt id", &value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IntegrationPromotionAttemptId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
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

/// Exact lane candidate and verification provenance included in one promotion preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPromotionLaneCandidate {
    pub lane_id: IntegrationLaneId,
    pub candidate: IntegrationLaneCandidate,
    pub verification_receipt_ids: Vec<String>,
}

/// Content-bound input used to generate one promotion preview.
#[derive(Debug, Clone)]
pub struct TaskPromotionPreviewInput {
    pub aggregate_diff_artifact_ref: String,
    pub aggregate_diff_digest: String,
    pub target: IntegrationPromotionTarget,
    pub verification_invalidation: Vec<String>,
    pub intent_binding: Option<String>,
    pub policy_digest: String,
    pub has_pending_approval: bool,
    pub has_executable_intent_refs: bool,
    pub created_at_unix_ms: u64,
}

/// Host-generated review payload for the final promotion barrier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPromotionPreview {
    pub task_id: TaskId,
    pub plan_id: IntegrationPlanId,
    pub plan_version: u32,
    pub ordered_lane_candidates: Vec<TaskPromotionLaneCandidate>,
    pub aggregate_diff_artifact_ref: String,
    pub aggregate_diff_digest: String,
    pub target: IntegrationPromotionTarget,
    pub verification_invalidation: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_binding: Option<String>,
    pub policy_digest: String,
    pub preview_digest: String,
    pub created_at_unix_ms: u64,
}

/// Append-only record of one exact promotion preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPromotionPreviewRecorded {
    pub preview: TaskPromotionPreview,
}

/// Exact product action identity for opening one current promotion review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskIntegrationReviewRequest {
    pub request_id: String,
    pub task_id: TaskId,
    pub plan_id: IntegrationPlanId,
    pub plan_version: u32,
    pub preview_digest: String,
}

impl TaskIntegrationReviewRequest {
    /// Binds a UI review action to one exact promotion preview.
    ///
    /// # Errors
    ///
    /// Returns an error when the preview is malformed.
    pub fn from_preview(preview: &TaskPromotionPreview) -> Result<Self> {
        preview.validate()?;
        Ok(Self {
            request_id: integration_review_request_id(preview),
            task_id: preview.task_id.clone(),
            plan_id: preview.plan_id.clone(),
            plan_version: preview.plan_version,
            preview_digest: preview.preview_digest.clone(),
        })
    }

    /// Rejects a stale or substituted UI action before artifact loading or promotion.
    ///
    /// # Errors
    ///
    /// Returns an error when any request identity differs from the exact preview.
    pub fn validate_for_preview(&self, preview: &TaskPromotionPreview) -> Result<()> {
        preview.validate()?;
        if self.request_id != integration_review_request_id(preview)
            || self.task_id != preview.task_id
            || self.plan_id != preview.plan_id
            || self.plan_version != preview.plan_version
            || self.preview_digest != preview.preview_digest
        {
            bail!("task integration review request is stale or belongs to another preview");
        }
        Ok(())
    }
}

/// Current product projection for one pending integration review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskIntegrationReviewProduct {
    pub request: TaskIntegrationReviewRequest,
    pub preview: TaskPromotionPreview,
}

/// Host-owned source that may authorize one exact promotion preview.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskPromotionAuthoritySource {
    UserIntegrationReview { review_id: String },
    ControlledAutoPostEffect { admission_id: String },
}

/// Single-use, content-bound authority for one promotion attempt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPromotionAuthority {
    pub source: TaskPromotionAuthoritySource,
    pub task_id: TaskId,
    pub plan_id: IntegrationPlanId,
    pub plan_version: u32,
    pub preview_digest: String,
    pub aggregate_diff_digest: String,
    pub target: IntegrationPromotionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent_binding: Option<String>,
    pub policy_digest: String,
    pub expires_at_unix_ms: u64,
    pub nonce: String,
}

/// Durable consumption barrier recorded before the first promotion effect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskPromotionAuthorityConsumed {
    pub attempt_id: IntegrationPromotionAttemptId,
    pub authority: TaskPromotionAuthority,
    pub consumed_at_unix_ms: u64,
}

/// Parent-scope verification bound to the authoritative promoted snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct TaskParentVerificationRecorded {
    pub attempt_id: IntegrationPromotionAttemptId,
    pub plan_id: IntegrationPlanId,
    pub preview_digest: String,
    pub promoted_snapshot_id: WorkspaceSnapshotId,
    pub policy_digest: String,
    pub verdict: VerificationVerdict,
    pub receipts: Vec<VerificationReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub recorded_at_unix_ms: u64,
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

impl IntegrationPromotionTarget {
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::WorkspaceApply { .. } => "workspace_apply",
            Self::GitRefAdvance { .. } => "git_ref_advance",
        }
    }
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

/// Runtime-owned recovery binding retained across a promotion crash window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationPromotionRecoveryBinding {
    pub owned_workspace_id: String,
    pub candidate_snapshot_id: WorkspaceSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_parent_snapshot_id: Option<WorkspaceSnapshotId>,
}

/// Append-only final promotion fact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct IntegrationPromotionRecorded {
    pub plan_id: IntegrationPlanId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<IntegrationPromotionAttemptId>,
    pub status: IntegrationPromotionStatus,
    pub preview_digest: String,
    pub target: IntegrationPromotionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_nonce: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<IntegrationPromotionEffect>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_binding: Option<IntegrationPromotionRecoveryBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub recorded_at_unix_ms: u64,
}

impl TaskPromotionPreview {
    /// Recomputes and validates the content digest carried by this preview.
    ///
    /// # Errors
    ///
    /// Returns an error when the preview is incomplete or its digest does not bind its content.
    pub fn validate(&self) -> Result<()> {
        if self.ordered_lane_candidates.is_empty() {
            bail!("task promotion preview has no lane candidates");
        }
        if self.aggregate_diff_artifact_ref.trim().is_empty() {
            bail!("task promotion preview aggregate artifact ref must not be empty");
        }
        validate_sha256_digest(
            "task promotion preview aggregate diff digest",
            &self.aggregate_diff_digest,
        )?;
        validate_policy_digest("task promotion preview policy digest", &self.policy_digest)?;
        validate_sha256_digest("task promotion preview digest", &self.preview_digest)?;
        if self.verification_invalidation.is_empty()
            || self
                .verification_invalidation
                .iter()
                .any(|value| value.trim().is_empty())
        {
            bail!("task promotion preview must invalidate explicit verification scopes");
        }
        if self.created_at_unix_ms == 0 {
            bail!("task promotion preview timestamp must not be zero");
        }
        let computed = task_promotion_preview_digest(self)?;
        if self.preview_digest != computed {
            bail!("task promotion preview digest does not match its content");
        }
        Ok(())
    }
}

impl TaskPromotionAuthority {
    /// Issues one user-review authority by copying every security-relevant preview binding.
    ///
    /// The runtime must call this only after the host resolves an exact integration review. A
    /// planner response, task plan, or ordinary tool approval is not a review identity.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid preview, empty review/nonce, or non-future expiry.
    pub fn from_user_integration_review(
        preview: &TaskPromotionPreview,
        review_id: impl Into<String>,
        expires_at_unix_ms: u64,
        nonce: impl Into<String>,
    ) -> Result<Self> {
        preview.validate()?;
        let review_id = review_id.into();
        let nonce = nonce.into();
        if review_id.trim().is_empty() {
            bail!("task promotion review id must not be empty");
        }
        validate_stable_id("task promotion authority nonce", &nonce)?;
        if expires_at_unix_ms <= preview.created_at_unix_ms {
            bail!("task promotion authority expiry must follow preview creation");
        }
        Ok(Self {
            source: TaskPromotionAuthoritySource::UserIntegrationReview { review_id },
            task_id: preview.task_id.clone(),
            plan_id: preview.plan_id.clone(),
            plan_version: preview.plan_version,
            preview_digest: preview.preview_digest.clone(),
            aggregate_diff_digest: preview.aggregate_diff_digest.clone(),
            target: preview.target.clone(),
            intent_binding: preview.intent_binding.clone(),
            policy_digest: preview.policy_digest.clone(),
            expires_at_unix_ms,
            nonce,
        })
    }

    /// Validates this authority against the exact latest preview at consumption time.
    ///
    /// # Errors
    ///
    /// Returns an error for expiry or any task/plan/content/target/policy binding mismatch.
    pub fn validate_for_preview(
        &self,
        preview: &TaskPromotionPreview,
        consumed_at_unix_ms: u64,
    ) -> Result<()> {
        preview.validate()?;
        validate_stable_id("task promotion authority nonce", &self.nonce)?;
        match &self.source {
            TaskPromotionAuthoritySource::UserIntegrationReview { review_id } => {
                if review_id.trim().is_empty() {
                    bail!("task promotion user review id must not be empty");
                }
            }
            TaskPromotionAuthoritySource::ControlledAutoPostEffect { admission_id } => {
                if admission_id.trim().is_empty() {
                    bail!("controlled-auto promotion admission id must not be empty");
                }
                bail!("controlled-auto promotion authority is unavailable until E05.17 is enabled");
            }
        }
        if consumed_at_unix_ms == 0 || consumed_at_unix_ms > self.expires_at_unix_ms {
            bail!("task promotion authority is expired or has an invalid consumption time");
        }
        if self.task_id != preview.task_id
            || self.plan_id != preview.plan_id
            || self.plan_version != preview.plan_version
            || self.preview_digest != preview.preview_digest
            || self.aggregate_diff_digest != preview.aggregate_diff_digest
            || self.target != preview.target
            || self.intent_binding != preview.intent_binding
            || self.policy_digest != preview.policy_digest
        {
            bail!("task promotion authority does not match the exact preview");
        }
        Ok(())
    }
}

impl TaskParentVerificationRecorded {
    /// Validates terminal parent-check evidence against the promoted snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error when the verdict is non-terminal, a receipt belongs to another snapshot,
    /// or a passed verdict lacks complete successful receipts.
    pub fn validate(&self) -> Result<()> {
        if self.promoted_snapshot_id.trim().is_empty()
            || self.preview_digest.trim().is_empty()
            || self.recorded_at_unix_ms == 0
        {
            bail!("task parent verification binding is incomplete");
        }
        validate_policy_digest(
            "task parent verification policy digest",
            &self.policy_digest,
        )?;
        if !self.verdict.is_terminal() {
            bail!("task parent verification verdict must be terminal");
        }
        if self.receipts.iter().any(|receipt| {
            receipt.binding.workspace_snapshot_id != self.promoted_snapshot_id
                || receipt.binding.verification_scope_hash.trim().is_empty()
        }) {
            bail!("task parent verification receipt belongs to another snapshot or scope");
        }
        if self.verdict == VerificationVerdict::Passed
            && (self.receipts.is_empty()
                || self.receipts.iter().any(|receipt| {
                    receipt.check_status != ReceiptStatus::Succeeded
                        || receipt.receipt.status != ReceiptStatus::Succeeded
                        || receipt.mutates_verification_scope
                        || receipt.binding.execution_backend.is_none()
                }))
        {
            bail!("passed task parent verification requires complete successful receipts");
        }
        Ok(())
    }
}

/// Builds one promotion preview only after every physical lane is ready and unambiguous.
///
/// # Errors
///
/// Returns an error for pending approval, incomplete/conflicted lanes, cleanup ambiguity, an
/// incompatible target, executable-intent ref advancement, or malformed content bindings.
pub fn build_task_promotion_preview(
    state: &IntegrationPlanState,
    input: TaskPromotionPreviewInput,
) -> Result<TaskPromotionPreview> {
    if state.inconsistent {
        bail!("integration plan projection is inconsistent");
    }
    if input.has_pending_approval {
        bail!("integration plan still has a pending approval");
    }
    if input.created_at_unix_ms == 0 {
        bail!("task promotion preview timestamp must not be zero");
    }
    if input.aggregate_diff_artifact_ref.trim().is_empty() {
        bail!("task promotion aggregate diff artifact ref must not be empty");
    }
    validate_sha256_digest(
        "task promotion aggregate diff digest",
        &input.aggregate_diff_digest,
    )?;
    validate_policy_digest("task promotion policy digest", &input.policy_digest)?;
    if input.verification_invalidation.is_empty()
        || input
            .verification_invalidation
            .iter()
            .any(|value| value.trim().is_empty())
    {
        bail!("task promotion must invalidate explicit verification scopes");
    }
    if input.has_executable_intent_refs
        && matches!(
            input.target,
            IntegrationPromotionTarget::GitRefAdvance { .. }
        )
    {
        bail!("executable intent refs require workspace_apply promotion");
    }
    match (&state.recorded.plan.base_representation, &input.target) {
        (
            _,
            IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id,
                ..
            },
        ) if expected_snapshot_id == &state.recorded.plan.base_snapshot_id => {}
        (
            IntegrationBaseRepresentation::CleanCommit { base_commit },
            IntegrationPromotionTarget::GitRefAdvance {
                target_ref,
                expected_old_oid,
                candidate_oid,
            },
        ) => {
            if target_ref.trim().is_empty() {
                bail!("task promotion target ref must not be empty");
            }
            validate_git_object_id("task promotion expected ref oid", expected_old_oid)?;
            validate_git_object_id("task promotion candidate oid", candidate_oid)?;
            if expected_old_oid != base_commit {
                bail!("task promotion ref target does not match the clean integration base");
            }
        }
        (IntegrationBaseRepresentation::SnapshotWorkspace { .. }, _) => {
            bail!("snapshot-workspace integration can only promote through workspace_apply");
        }
        (_, IntegrationPromotionTarget::WorkspaceApply { .. }) => {
            bail!("task promotion workspace target snapshot does not match the integration base");
        }
        (IntegrationBaseRepresentation::Unknown, _) => {
            bail!("task promotion requires a complete integration base");
        }
    }

    let mut ordered_lane_candidates = Vec::with_capacity(state.recorded.plan.lanes.len());
    for lane in &state.recorded.plan.lanes {
        let lifecycle = state
            .lifecycle_lanes
            .get(&lane.lane_id)
            .ok_or_else(|| anyhow::anyhow!("integration lane has no physical lifecycle"))?;
        if lifecycle.inconsistent {
            bail!("integration lane projection is inconsistent");
        }
        let terminal = lifecycle
            .terminal
            .as_ref()
            .filter(|terminal| terminal.status == IntegrationLaneStatus::Ready)
            .ok_or_else(|| anyhow::anyhow!("integration lane is not terminal-ready"))?;
        let verification = lifecycle
            .verification
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("integration lane has no scoped verification"))?;
        if terminal.candidate.as_ref() != Some(&verification.candidate) {
            bail!("integration lane terminal candidate does not match verification");
        }
        let cleanup = lifecycle
            .cleanup
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("integration lane cleanup disposition is unknown"))?;
        if cleanup.status == IntegrationLaneCleanupStatus::Failed {
            bail!("integration lane cleanup failed");
        }
        let verification_receipt_ids = verification
            .verification_receipts
            .iter()
            .map(|receipt| receipt.receipt.receipt_id.clone())
            .collect::<Vec<_>>();
        if verification_receipt_ids.is_empty()
            || verification_receipt_ids
                .iter()
                .any(|receipt_id| receipt_id.trim().is_empty())
        {
            bail!("integration lane verification receipt identity is incomplete");
        }
        ordered_lane_candidates.push(TaskPromotionLaneCandidate {
            lane_id: lane.lane_id.clone(),
            candidate: verification.candidate.clone(),
            verification_receipt_ids,
        });
    }

    let mut preview = TaskPromotionPreview {
        task_id: state.recorded.plan.task_id.clone(),
        plan_id: state.recorded.plan.plan_id.clone(),
        plan_version: state.recorded.plan.plan_version,
        ordered_lane_candidates,
        aggregate_diff_artifact_ref: input.aggregate_diff_artifact_ref,
        aggregate_diff_digest: input.aggregate_diff_digest,
        target: input.target,
        verification_invalidation: input.verification_invalidation,
        intent_binding: input.intent_binding,
        policy_digest: input.policy_digest,
        preview_digest: String::new(),
        created_at_unix_ms: input.created_at_unix_ms,
    };
    preview.preview_digest = task_promotion_preview_digest(&preview)?;
    preview.validate()?;
    Ok(preview)
}

/// Projects at most one current, unconsumed promotion review action.
///
/// A later task, task-plan version, integration plan, consumed authority or promotion terminal
/// suppresses the old action. Callers must re-run this projection immediately before reading the
/// artifact or issuing authority.
#[must_use]
pub fn task_integration_review_product(
    entries: &[SessionLogEntry],
) -> Option<TaskIntegrationReviewProduct> {
    let integration = IntegrationProjection::from_entries(entries);
    let plan_id = integration.latest_plan_id.as_ref()?;
    let state = integration.plans.get(plan_id)?;
    if state.inconsistent {
        return None;
    }
    let task_projection = crate::TaskStateProjection::from_entries(entries);
    if let Some(task) = task_projection.latest_task()
        && (task.task_id != state.recorded.plan.task_id
            || task.latest_plan_version != Some(state.recorded.plan.plan_version))
    {
        return None;
    }
    let preview = entries.iter().rev().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::TaskPromotionPreviewRecorded(recorded))
            if recorded.preview.plan_id == *plan_id
                && recorded.preview.task_id == state.recorded.plan.task_id
                && recorded.preview.plan_version == state.recorded.plan.plan_version =>
        {
            Some(recorded.preview.clone())
        }
        _ => None,
    })?;
    if preview.validate().is_err()
        || !state
            .promotion_previews
            .contains_key(&preview.preview_digest)
        || state
            .consumed_promotion_authorities
            .values()
            .any(|consumed| consumed.authority.preview_digest == preview.preview_digest)
        || state
            .promotions
            .iter()
            .any(|promotion| promotion.preview_digest == preview.preview_digest)
    {
        return None;
    }
    let request = TaskIntegrationReviewRequest::from_preview(&preview).ok()?;
    Some(TaskIntegrationReviewProduct { request, preview })
}

/// Latest replayed state for one integration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationPlanState {
    pub recorded: IntegrationPlanRecorded,
    pub lanes: BTreeMap<IntegrationLaneId, IntegrationLaneChanged>,
    pub lifecycle_lanes: BTreeMap<IntegrationLaneId, IntegrationLaneLifecycleState>,
    pub promotion_previews: BTreeMap<String, TaskPromotionPreview>,
    pub consumed_promotion_authorities: BTreeMap<String, TaskPromotionAuthorityConsumed>,
    pub promotions: Vec<IntegrationPromotionRecorded>,
    pub parent_verifications:
        BTreeMap<IntegrationPromotionAttemptId, TaskParentVerificationRecorded>,
    pub inconsistent: bool,
}

impl IntegrationPlanState {
    /// Returns the promoted attempt whose parent-scope verification passed.
    ///
    /// Child and lane receipts intentionally do not satisfy this gate. The latest physical
    /// promotion must be terminal-success and bind a passed parent verification record.
    #[must_use]
    pub fn synthesis_ready_attempt(&self) -> Option<&IntegrationPromotionAttemptId> {
        if self.inconsistent {
            return None;
        }
        let promotion = self.promotions.last()?;
        if promotion.status != IntegrationPromotionStatus::Promoted {
            return None;
        }
        let attempt_id = promotion.attempt_id.as_ref()?;
        self.parent_verifications
            .get(attempt_id)
            .is_some_and(|verification| {
                verification.verdict == VerificationVerdict::Passed
                    && verification.preview_digest == promotion.preview_digest
            })
            .then_some(attempt_id)
    }
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
                                promotion_previews: BTreeMap::new(),
                                consumed_promotion_authorities: BTreeMap::new(),
                                promotions: Vec::new(),
                                parent_verifications: BTreeMap::new(),
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
            ControlEntry::TaskPromotionPreviewRecorded(entry) => {
                let Some(state) = self.plans.get_mut(&entry.preview.plan_id) else {
                    return;
                };
                if entry.preview.validate().is_err()
                    || !promotion_preview_matches_plan_state(&entry.preview, state)
                {
                    state.inconsistent = true;
                    return;
                }
                match state.promotion_previews.get(&entry.preview.preview_digest) {
                    Some(existing) if existing != &entry.preview => state.inconsistent = true,
                    Some(_) => {}
                    None => {
                        state
                            .promotion_previews
                            .insert(entry.preview.preview_digest.clone(), entry.preview.clone());
                    }
                }
            }
            ControlEntry::TaskPromotionAuthorityConsumed(entry) => {
                let Some(state) = self.plans.get_mut(&entry.authority.plan_id) else {
                    return;
                };
                let valid = state
                    .promotion_previews
                    .get(&entry.authority.preview_digest)
                    .is_some_and(|preview| {
                        entry
                            .authority
                            .validate_for_preview(preview, entry.consumed_at_unix_ms)
                            .is_ok()
                    })
                    && !state
                        .consumed_promotion_authorities
                        .values()
                        .any(|consumed| consumed.attempt_id == entry.attempt_id);
                if !valid
                    || state
                        .consumed_promotion_authorities
                        .contains_key(&entry.authority.nonce)
                {
                    state.inconsistent = true;
                    return;
                }
                state
                    .consumed_promotion_authorities
                    .insert(entry.authority.nonce.clone(), entry.clone());
            }
            ControlEntry::IntegrationPromotionRecorded(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let protocol_valid = match (&entry.attempt_id, &entry.authority_nonce) {
                    (None, None) => true,
                    (Some(attempt_id), Some(nonce)) => {
                        protocol_promotion_matches_state(state, entry, attempt_id, nonce)
                    }
                    _ => false,
                };
                if entry.preview_digest.trim().is_empty()
                    || !promotion_effect_matches_target(entry)
                    || !protocol_valid
                {
                    state.inconsistent = true;
                }
                state.promotions.push(entry.clone());
            }
            ControlEntry::TaskParentVerificationRecorded(entry) => {
                let Some(state) = self.plans.get_mut(&entry.plan_id) else {
                    return;
                };
                let promoted = state.promotions.iter().rev().find(|promotion| {
                    promotion.attempt_id.as_ref() == Some(&entry.attempt_id)
                        && promotion.status == IntegrationPromotionStatus::Promoted
                });
                let valid = entry.validate().is_ok()
                    && promoted.is_some_and(|promotion| {
                        promotion.preview_digest == entry.preview_digest
                            && state
                                .promotion_previews
                                .get(&entry.preview_digest)
                                .is_some_and(|preview| {
                                    preview.policy_digest == entry.policy_digest
                                        && preview.target == promotion.target
                                })
                    });
                if !valid {
                    state.inconsistent = true;
                    return;
                }
                match state.parent_verifications.get(&entry.attempt_id) {
                    Some(existing) if existing != entry => state.inconsistent = true,
                    Some(_) => {}
                    None => {
                        state
                            .parent_verifications
                            .insert(entry.attempt_id.clone(), entry.clone());
                    }
                }
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

fn promotion_preview_matches_plan_state(
    preview: &TaskPromotionPreview,
    state: &IntegrationPlanState,
) -> bool {
    if preview.task_id != state.recorded.plan.task_id
        || preview.plan_id != state.recorded.plan.plan_id
        || preview.plan_version != state.recorded.plan.plan_version
        || preview.ordered_lane_candidates.len() != state.recorded.plan.lanes.len()
    {
        return false;
    }
    let target_matches = match (&state.recorded.plan.base_representation, &preview.target) {
        (
            _,
            IntegrationPromotionTarget::WorkspaceApply {
                expected_snapshot_id,
                ..
            },
        ) => expected_snapshot_id == &state.recorded.plan.base_snapshot_id,
        (
            IntegrationBaseRepresentation::CleanCommit { base_commit },
            IntegrationPromotionTarget::GitRefAdvance {
                target_ref,
                expected_old_oid,
                candidate_oid,
            },
        ) => {
            !target_ref.trim().is_empty()
                && expected_old_oid == base_commit
                && validate_git_object_id("promotion preview expected oid", expected_old_oid)
                    .is_ok()
                && validate_git_object_id("promotion preview candidate oid", candidate_oid).is_ok()
        }
        _ => false,
    };
    target_matches
        && state
            .recorded
            .plan
            .lanes
            .iter()
            .zip(preview.ordered_lane_candidates.iter())
            .all(|(lane, candidate)| {
                if lane.lane_id != candidate.lane_id {
                    return false;
                }
                state
                    .lifecycle_lanes
                    .get(&lane.lane_id)
                    .is_some_and(|lifecycle| {
                        let receipt_ids = lifecycle
                            .verification
                            .as_ref()
                            .map(|verification| {
                                verification
                                    .verification_receipts
                                    .iter()
                                    .map(|receipt| receipt.receipt.receipt_id.as_str())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        !lifecycle.inconsistent
                            && lifecycle.terminal.as_ref().is_some_and(|terminal| {
                                terminal.status == IntegrationLaneStatus::Ready
                                    && terminal.candidate.as_ref() == Some(&candidate.candidate)
                            })
                            && lifecycle.cleanup.as_ref().is_some_and(|cleanup| {
                                cleanup.status != IntegrationLaneCleanupStatus::Failed
                            })
                            && receipt_ids
                                == candidate
                                    .verification_receipt_ids
                                    .iter()
                                    .map(String::as_str)
                                    .collect::<Vec<_>>()
                    })
            })
}

fn protocol_promotion_matches_state(
    state: &IntegrationPlanState,
    entry: &IntegrationPromotionRecorded,
    attempt_id: &IntegrationPromotionAttemptId,
    nonce: &str,
) -> bool {
    if entry.recorded_at_unix_ms == 0 {
        return false;
    }
    let Some(consumed) = state.consumed_promotion_authorities.get(nonce) else {
        return false;
    };
    if &consumed.attempt_id != attempt_id
        || consumed.authority.preview_digest != entry.preview_digest
        || consumed.authority.target != entry.target
        || entry.recorded_at_unix_ms < consumed.consumed_at_unix_ms
    {
        return false;
    }
    let prior = state
        .promotions
        .iter()
        .filter(|promotion| promotion.attempt_id.as_ref() == Some(attempt_id))
        .collect::<Vec<_>>();
    match prior.as_slice() {
        [] => {
            entry.status == IntegrationPromotionStatus::Prepared
                && entry.effect.is_none()
                && entry.reason.is_none()
        }
        [prepared] => {
            prepared.status == IntegrationPromotionStatus::Prepared
                && prepared.preview_digest == entry.preview_digest
                && prepared.target == entry.target
                && prepared.authority_nonce.as_deref() == Some(nonce)
                && prepared.recovery_binding == entry.recovery_binding
                && entry.status != IntegrationPromotionStatus::Prepared
        }
        _ => false,
    }
}

fn promotion_effect_matches_target(entry: &IntegrationPromotionRecorded) -> bool {
    if entry.recovery_binding.as_ref().is_some_and(|binding| {
        binding.owned_workspace_id.trim().is_empty()
            || binding.candidate_snapshot_id.trim().is_empty()
            || binding
                .expected_parent_snapshot_id
                .as_ref()
                .is_some_and(|snapshot_id| snapshot_id.trim().is_empty())
    }) {
        return false;
    }
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

fn task_promotion_preview_digest(preview: &TaskPromotionPreview) -> Result<String> {
    let value = serde_json::json!({
        "schema_version": 1,
        "task_id": preview.task_id,
        "plan_id": preview.plan_id,
        "plan_version": preview.plan_version,
        "ordered_lane_candidates": preview.ordered_lane_candidates,
        "aggregate_diff_artifact_ref": preview.aggregate_diff_artifact_ref,
        "aggregate_diff_digest": preview.aggregate_diff_digest,
        "target": preview.target,
        "verification_invalidation": preview.verification_invalidation,
        "intent_binding": preview.intent_binding,
        "policy_digest": preview.policy_digest,
        "created_at_unix_ms": preview.created_at_unix_ms,
    });
    Ok(format!(
        "sha256:{}",
        sha256_hex(&serde_json::to_vec(&value)?)
    ))
}

fn integration_review_request_id(preview: &TaskPromotionPreview) -> String {
    format!(
        "integration-review-{}",
        sha256_hex(
            format!(
                "{}:{}:{}:{}",
                preview.task_id.as_str(),
                preview.plan_id.as_str(),
                preview.plan_version,
                preview.preview_digest
            )
            .as_bytes()
        )
    )
}

const fn is_zero(value: &u64) -> bool {
    *value == 0
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

fn validate_policy_digest(label: &str, value: &str) -> Result<()> {
    let value = value
        .strip_prefix("sha256:jcs-v1:")
        .or_else(|| value.strip_prefix("sha256:"))
        .ok_or_else(|| anyhow::anyhow!("{label} must use a supported sha256 prefix"))?;
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("{label} must contain exactly 64 hexadecimal characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/integration_tests.rs"]
mod tests;
