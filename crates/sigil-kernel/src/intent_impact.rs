use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    INTENT_CANONICAL_DIGEST_PREFIX, INTENT_CONTRACT_SCHEMA_VERSION, IntentAcceptanceCriterionV1,
    IntentAcceptanceKind, IntentAdmissionContextV1, IntentAdmissionWriteOutcomeV1,
    IntentApplicationState, IntentDigest, IntentDigestDomain, IntentId, IntentOperationId,
    IntentOperationKind, IntentOperationPreviewV1, IntentPlanV1, IntentProposalCriterionV1,
    IntentSourceV1, IntentStackId, IntentStackProjectionV1, IntentStackVersion,
    IntentVerificationImpact, IntentVerificationImpactV1, IntentVersionRef, JsonlSessionStore,
    MAX_INTENT_CRITERIA, MAX_INTENT_DEPENDENCIES, MAX_INTENT_STATEMENT_BYTES,
    MAX_INTENT_TITLE_BYTES, Session, TaskPlanEntry, agent_invocation_workspace_snapshot_id,
    canonical_intent_digest, stable_workspace_id,
};

/// Untrusted, digest-bound proposal for one immutable intent definition successor.
///
/// It does not carry acceptance authority, execution authority, file effects, or mutation bytes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentRevisionProposalV1 {
    pub schema_version: u16,
    pub proposal_id: String,
    pub source_turn_id: String,
    pub target_intent_ref: IntentVersionRef,
    pub title: String,
    pub statement: String,
    pub acceptance_criteria: Vec<IntentProposalCriterionV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<IntentId>,
    pub proposal_digest: IntentDigest,
}

impl IntentRevisionProposalV1 {
    /// Builds a content-bound proposal without granting acceptance.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed or unbounded proposal content.
    pub fn new(
        proposal_id: impl Into<String>,
        source_turn_id: impl Into<String>,
        target_intent_ref: IntentVersionRef,
        title: impl Into<String>,
        statement: impl Into<String>,
        acceptance_criteria: Vec<IntentProposalCriterionV1>,
        depends_on: Vec<IntentId>,
    ) -> Result<Self> {
        let mut proposal = Self {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            proposal_id: proposal_id.into(),
            source_turn_id: source_turn_id.into(),
            target_intent_ref,
            title: title.into(),
            statement: statement.into(),
            acceptance_criteria,
            depends_on,
            proposal_digest: zero_digest()?,
        };
        proposal.proposal_digest = proposal.computed_digest()?;
        proposal.validate_contract()?;
        Ok(proposal)
    }

    /// Computes the domain-separated canonical proposal digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the proposal cannot be represented as canonical JSON.
    pub fn computed_digest(&self) -> Result<IntentDigest> {
        let mut value =
            serde_json::to_value(self).context("failed to serialize intent revision proposal")?;
        value
            .as_object_mut()
            .context("intent revision proposal must serialize as an object")?
            .remove("proposal_digest");
        canonical_intent_digest(IntentDigestDomain::Proposal, &value)
    }

    /// Returns the immutable definition ref that acceptance would create.
    ///
    /// # Errors
    ///
    /// Returns an error if the next immutable version cannot be represented.
    pub fn replacement_ref(&self) -> Result<IntentVersionRef> {
        IntentVersionRef::new(
            self.target_intent_ref.intent_id.clone(),
            self.target_intent_ref.version.saturating_add(1),
        )
    }

    /// Validates proposal shape and digest without granting authority.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, unbounded, duplicate, or digest-mismatched content.
    pub fn validate_contract(&self) -> Result<()> {
        if self.schema_version != INTENT_CONTRACT_SCHEMA_VERSION {
            bail!("unsupported intent revision proposal schema");
        }
        validate_identity("intent revision proposal id", &self.proposal_id)?;
        validate_identity("intent revision source turn id", &self.source_turn_id)?;
        self.target_intent_ref.validate()?;
        validate_text("intent revision title", &self.title, MAX_INTENT_TITLE_BYTES)?;
        validate_text(
            "intent revision statement",
            &self.statement,
            MAX_INTENT_STATEMENT_BYTES,
        )?;
        if self.acceptance_criteria.is_empty()
            || self.acceptance_criteria.len() > MAX_INTENT_CRITERIA
        {
            bail!("intent revision requires a bounded non-empty criterion set");
        }
        if self.depends_on.len() > MAX_INTENT_DEPENDENCIES {
            bail!("intent revision contains too many dependencies");
        }
        let mut criterion_aliases = BTreeSet::new();
        for criterion in &self.acceptance_criteria {
            validate_identity(
                "intent revision criterion alias",
                &criterion.criterion_alias,
            )?;
            validate_text(
                "intent revision criterion",
                &criterion.statement,
                MAX_INTENT_STATEMENT_BYTES,
            )?;
            if !criterion_aliases.insert(criterion.criterion_alias.as_str()) {
                bail!("intent revision repeats a criterion alias");
            }
        }
        let mut dependencies = BTreeSet::new();
        for dependency in &self.depends_on {
            if dependency == &self.target_intent_ref.intent_id
                || !dependencies.insert(dependency.as_str())
            {
                bail!("intent revision contains a self or duplicate dependency");
            }
        }
        if self.computed_digest()? != self.proposal_digest {
            bail!("intent revision proposal canonical digest mismatch");
        }
        Ok(())
    }
}

/// Read-only application-state effect for one member of a dependency closure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentApplicationImpactV1 {
    pub intent_ref: IntentVersionRef,
    pub application_state: IntentApplicationState,
}

/// Bounded read-only revise/replace impact. The embedded operation preview has no file effects and
/// cannot be converted into mutation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct IntentImpactPreviewV1 {
    pub operation: IntentOperationPreviewV1,
    pub application_impacts: Vec<IntentApplicationImpactV1>,
}

impl IntentImpactPreviewV1 {
    /// Validates the closure/state projection and the canonical embedded preview.
    ///
    /// # Errors
    ///
    /// Returns an error when the preview is effectful, malformed, or internally inconsistent.
    pub fn validate_contract(&self) -> Result<()> {
        self.operation.validate_contract()?;
        if !matches!(
            self.operation.operation_kind,
            IntentOperationKind::ReviseImpactPreview | IntentOperationKind::ReplaceImpactPreview
        ) || !self.operation.file_effects.is_empty()
            || self.application_impacts.len() != self.operation.target_intents.len()
        {
            bail!("intent impact preview is not a read-only closure preview");
        }
        for (impact, target) in self
            .application_impacts
            .iter()
            .zip(&self.operation.target_intents)
        {
            if &impact.intent_ref != target
                || !matches!(
                    impact.application_state,
                    IntentApplicationState::NeedsReview | IntentApplicationState::NeedsRebuild
                )
            {
                bail!("intent impact preview contains an invalid application transition");
            }
        }
        Ok(())
    }
}

/// Non-serializable host authority binding an exact revision proposal and read-only preview to an
/// explicit user confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentRevisionAuthorityV1 {
    source_turn_id: String,
    authority_event_id: String,
    proposal_digest: IntentDigest,
    preview_digest: IntentDigest,
}

impl IntentRevisionAuthorityV1 {
    /// Captures explicit user confirmation for one exact proposal/preview pair.
    ///
    /// # Errors
    ///
    /// Returns an error when either contract is invalid or the authority identity is unbounded.
    pub fn explicit_user_confirmation(
        proposal: &IntentRevisionProposalV1,
        preview: &IntentImpactPreviewV1,
        authority_event_id: impl Into<String>,
    ) -> Result<Self> {
        proposal.validate_contract()?;
        preview.validate_contract()?;
        if preview.operation.operation_kind != IntentOperationKind::ReviseImpactPreview {
            bail!("intent revision authority requires a revise impact preview");
        }
        let authority_event_id = authority_event_id.into();
        validate_identity(
            "intent revision acceptance authority id",
            &authority_event_id,
        )?;
        Ok(Self {
            source_turn_id: proposal.source_turn_id.clone(),
            authority_event_id,
            proposal_digest: proposal.proposal_digest.clone(),
            preview_digest: preview.operation.preview_digest.clone(),
        })
    }
}

/// Exact, non-serializable proof required to adopt fork provenance into a new stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentAdoptionAuthorityV1 {
    source_session_id: String,
    destination_session_id: String,
    source_plan_digest: IntentDigest,
    source_workspace_id: String,
    destination_workspace_id: String,
    source_snapshot_id: String,
    destination_snapshot_id: String,
    branch_lineage_digest: String,
    source_turn_id: String,
    authority_event_id: String,
    new_stack_id: IntentStackId,
}

impl IntentAdoptionAuthorityV1 {
    /// Captures exact source/destination workspace, branch-lineage, snapshot and fork identities.
    ///
    /// This is a host action, not provider JSON. Different branch lineage or tracked bytes fail
    /// before authority is minted.
    ///
    /// # Errors
    ///
    /// Returns an error for stale fork/plan state, mismatched branch lineage, incomplete tracked
    /// snapshots, workspace drift, or invalid authority identity.
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        source: &Session,
        destination: &Session,
        source_workspace_root: impl AsRef<Path>,
        destination_workspace_root: impl AsRef<Path>,
        source_branch_lineage_digest: impl Into<String>,
        destination_branch_lineage_digest: impl Into<String>,
        source_turn_id: impl Into<String>,
        authority_event_id: impl Into<String>,
        new_stack_id: IntentStackId,
    ) -> Result<Self> {
        let source_branch_lineage_digest = source_branch_lineage_digest.into();
        let destination_branch_lineage_digest = destination_branch_lineage_digest.into();
        validate_identity(
            "source Intent branch lineage digest",
            &source_branch_lineage_digest,
        )?;
        validate_identity(
            "destination Intent branch lineage digest",
            &destination_branch_lineage_digest,
        )?;
        if source_branch_lineage_digest != destination_branch_lineage_digest {
            bail!("Intent adoption branch lineage does not match");
        }
        let source_projection = source.intent_stack_projection()?;
        let source_plan = source_projection
            .latest_accepted_plan()
            .context("Intent adoption source has no accepted plan")?;
        let destination_projection = destination.intent_stack_projection()?;
        if destination_projection.fork_source_session_id() != Some(source.session_scope_id())
            || destination_projection.latest_accepted_plan().is_some()
        {
            bail!("Intent adoption destination is not an unused exact conversation fork");
        }
        let source_workspace_id = stable_workspace_id(source_workspace_root.as_ref())?;
        let destination_workspace_id = stable_workspace_id(destination_workspace_root.as_ref())?;
        if source_workspace_id != source_plan.plan.workspace_id {
            bail!("Intent adoption source workspace is out of scope");
        }
        let source_snapshot_id =
            agent_invocation_workspace_snapshot_id(source_workspace_root.as_ref())?;
        let destination_snapshot_id =
            agent_invocation_workspace_snapshot_id(destination_workspace_root.as_ref())?;
        let source_turn_id = source_turn_id.into();
        let authority_event_id = authority_event_id.into();
        validate_identity("Intent adoption source turn id", &source_turn_id)?;
        validate_identity("Intent adoption authority event id", &authority_event_id)?;
        Ok(Self {
            source_session_id: source.session_scope_id().to_owned(),
            destination_session_id: destination.session_scope_id().to_owned(),
            source_plan_digest: source_plan.plan.plan_digest.clone(),
            source_workspace_id,
            destination_workspace_id,
            source_snapshot_id,
            destination_snapshot_id,
            branch_lineage_digest: source_branch_lineage_digest,
            source_turn_id,
            authority_event_id,
            new_stack_id,
        })
    }
}

/// Computes a read-only impact for one proposed semantic revision.
///
/// # Errors
///
/// Returns an error for stale proposal/stack/workspace identity or unavailable durable evidence.
pub fn preview_intent_revision(
    session: &Session,
    workspace_root: impl AsRef<Path>,
    proposal: &IntentRevisionProposalV1,
) -> Result<IntentImpactPreviewV1> {
    proposal.validate_contract()?;
    preview_impact(
        session,
        workspace_root.as_ref(),
        &proposal.target_intent_ref,
        IntentOperationKind::ReviseImpactPreview,
        Some(proposal.proposal_digest.as_str()),
    )
}

/// Computes a read-only replacement/rebuild impact without creating mutation authority.
///
/// # Errors
///
/// Returns an error for a stale target, out-of-scope workspace, or invalid durable projection.
pub fn preview_intent_replace(
    session: &Session,
    workspace_root: impl AsRef<Path>,
    intent_ref: &IntentVersionRef,
) -> Result<IntentImpactPreviewV1> {
    preview_impact(
        session,
        workspace_root.as_ref(),
        intent_ref,
        IntentOperationKind::ReplaceImpactPreview,
        None,
    )
}

/// Accepts a revision as a new immutable plan version without modifying workspace files.
///
/// The target becomes `needs_rebuild`; transitive downstream intents become `needs_review`.
/// Existing receipts remain historical and are never reused for the successor definition.
///
/// # Errors
///
/// Returns an error when authority, proposal, preview, replan, workspace, or durable predecessor
/// state is stale or inconsistent.
pub fn accept_intent_revision(
    session: &mut Session,
    workspace_root: impl AsRef<Path>,
    proposal: &IntentRevisionProposalV1,
    preview: &IntentImpactPreviewV1,
    authority: &IntentRevisionAuthorityV1,
    task_plan: Option<TaskPlanEntry>,
) -> Result<IntentAdmissionWriteOutcomeV1> {
    let workspace_root = workspace_root.as_ref();
    proposal.validate_contract()?;
    preview.validate_contract()?;
    if authority.source_turn_id != proposal.source_turn_id
        || authority.proposal_digest != proposal.proposal_digest
        || authority.preview_digest != preview.operation.preview_digest
    {
        bail!("Intent revision authority does not match its proposal and preview");
    }
    let records = durable_records(session)?;
    let projection = IntentStackProjectionV1::from_records(&records)?;
    let accepted = projection
        .accepted_plan(preview.operation.stack_version)
        .context("Intent revision predecessor plan is unavailable")?;
    if accepted.plan.stack_id != preview.operation.stack_id {
        bail!("Intent revision preview belongs to another stack");
    }
    let target_index = accepted
        .plan
        .intents
        .iter()
        .position(|intent| intent.intent_ref == proposal.target_intent_ref)
        .context("Intent revision target is stale")?;
    let replacement_ref = proposal.replacement_ref()?;
    let context = IntentAdmissionContextV1 {
        stack_id: accepted.plan.stack_id.clone(),
        stack_version: IntentStackVersion::new(
            accepted.plan.stack_version.get().saturating_add(1),
        )?,
        workspace_id: accepted.plan.workspace_id.clone(),
        source_session_id: accepted.plan.source_session_id.clone(),
    };
    let criteria = proposal
        .acceptance_criteria
        .iter()
        .map(|criterion| {
            Ok(IntentAcceptanceCriterionV1 {
                criterion_id: crate::intent_admission::runtime_criterion_id(
                    &context,
                    replacement_ref.intent_id.as_str(),
                    &criterion.criterion_alias,
                )?,
                statement: criterion.statement.clone(),
                required: criterion.required,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let prior = &accepted.plan.intents[target_index];
    let source = match &prior.source {
        IntentSourceV1::UserTurn { .. } => IntentSourceV1::UserTurn {
            source_turn_id: proposal.source_turn_id.clone(),
        },
        IntentSourceV1::AcceptedSuggestion { .. } => IntentSourceV1::AcceptedSuggestion {
            source_turn_id: proposal.source_turn_id.clone(),
            proposal_digest: proposal.proposal_digest.clone(),
        },
        IntentSourceV1::TrustedSpec { .. } => {
            bail!("trusted-spec revision requires content-bound spec decision authority")
        }
    };
    let mut intents = accepted.plan.intents.clone();
    intents[target_index] = crate::IntentDefinitionV1 {
        intent_ref: replacement_ref,
        title: proposal.title.clone(),
        statement: proposal.statement.clone(),
        acceptance_criteria: criteria,
        depends_on: proposal.depends_on.clone(),
        source,
        supersedes: Some(proposal.target_intent_ref.clone()),
    };
    let mut plan = IntentPlanV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        stack_id: accepted.plan.stack_id.clone(),
        stack_version: context.stack_version,
        workspace_id: accepted.plan.workspace_id.clone(),
        source_session_id: accepted.plan.source_session_id.clone(),
        kind: accepted.plan.kind,
        intents,
        plan_digest: zero_digest()?,
    };
    plan.plan_digest = plan.computed_digest()?;
    plan.validate_contract()?;
    let admission = crate::intent_admission::build_successor_admission(
        plan,
        IntentAcceptanceKind::ExplicitUserConfirmation,
        proposal.source_turn_id.clone(),
        authority.authority_event_id.clone(),
    )?;
    let exact_retry = projection.latest_accepted_plan().is_some_and(|current| {
        current.plan == *admission.plan()
            && current.acceptance_kind == admission.acceptance_kind()
            && current.source_turn_id == proposal.source_turn_id
            && current.acceptance_authority_id == authority.authority_event_id
    });
    if !exact_retry {
        let current_preview = preview_intent_revision(session, workspace_root, proposal)?;
        if &current_preview != preview {
            bail!("Intent revision impact changed before acceptance");
        }
    }
    crate::append_successor_intent_plan_admission(session, &admission, task_plan)
}

/// Explicitly adopts read-only fork provenance into a new initial stack bound to exact current
/// workspace identity. No execution, layer, receipt, operation, or mutation authority is copied.
///
/// # Errors
///
/// Returns an error when fork, source plan, workspace, branch lineage, tracked snapshot, or
/// acceptance authority no longer matches the captured proof.
pub fn adopt_forked_intent_stack(
    source: &Session,
    destination: &Session,
    source_workspace_root: impl AsRef<Path>,
    destination_workspace_root: impl AsRef<Path>,
    source_branch_lineage_digest: &str,
    destination_branch_lineage_digest: &str,
    authority: &IntentAdoptionAuthorityV1,
) -> Result<IntentAdmissionWriteOutcomeV1> {
    if authority.source_session_id != source.session_scope_id()
        || authority.destination_session_id != destination.session_scope_id()
    {
        bail!("Intent adoption authority belongs to another session pair");
    }
    let source_projection = source.intent_stack_projection()?;
    let source_plan = source_projection
        .latest_accepted_plan()
        .context("Intent adoption source has no accepted plan")?;
    let destination_projection = destination.intent_stack_projection()?;
    if destination_projection.fork_source_session_id() != Some(authority.source_session_id.as_str())
        || source_plan.plan.plan_digest != authority.source_plan_digest
    {
        bail!("Intent adoption durable provenance changed");
    }
    let source_workspace_id = stable_workspace_id(source_workspace_root.as_ref())?;
    let destination_workspace_id = stable_workspace_id(destination_workspace_root.as_ref())?;
    let source_snapshot_id =
        agent_invocation_workspace_snapshot_id(source_workspace_root.as_ref())?;
    let destination_snapshot_id =
        agent_invocation_workspace_snapshot_id(destination_workspace_root.as_ref())?;
    validate_identity(
        "current source Intent branch lineage digest",
        source_branch_lineage_digest,
    )?;
    validate_identity(
        "current destination Intent branch lineage digest",
        destination_branch_lineage_digest,
    )?;
    if source_workspace_id != authority.source_workspace_id
        || destination_workspace_id != authority.destination_workspace_id
        || source_snapshot_id != authority.source_snapshot_id
        || destination_snapshot_id != authority.destination_snapshot_id
        || source_branch_lineage_digest != authority.branch_lineage_digest
        || destination_branch_lineage_digest != authority.branch_lineage_digest
    {
        bail!("Intent adoption workspace, branch, or snapshot proof changed");
    }
    let mut intents = source_plan.plan.intents.clone();
    for intent in &mut intents {
        intent.supersedes = None;
    }
    let mut plan = IntentPlanV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        stack_id: authority.new_stack_id.clone(),
        stack_version: IntentStackVersion::new(1)?,
        workspace_id: destination_workspace_id,
        source_session_id: destination.session_scope_id().to_owned(),
        kind: source_plan.plan.kind,
        intents,
        plan_digest: zero_digest()?,
    };
    plan.plan_digest = plan.computed_digest()?;
    plan.validate_contract()?;
    let admission = crate::intent_admission::build_successor_admission(
        plan,
        IntentAcceptanceKind::ExplicitUserConfirmation,
        authority.source_turn_id.clone(),
        authority.authority_event_id.clone(),
    )?;
    if let Some(current) = destination_projection.latest_accepted_plan()
        && (current.plan != *admission.plan()
            || current.acceptance_kind != admission.acceptance_kind()
            || current.source_turn_id != authority.source_turn_id
            || current.acceptance_authority_id != authority.authority_event_id
            || current.task_plan_binding.is_some())
    {
        bail!("Intent adoption destination already contains another accepted plan");
    }
    crate::intent_admission::append_adopted_intent_plan_admission(
        destination,
        &admission,
        &authority.source_session_id,
    )
}

fn preview_impact(
    session: &Session,
    workspace_root: &Path,
    intent_ref: &IntentVersionRef,
    operation_kind: IntentOperationKind,
    proposal_digest: Option<&str>,
) -> Result<IntentImpactPreviewV1> {
    let records = durable_records(session)?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let accepted = admission
        .latest_accepted_plan()
        .context("Intent impact preview requires an accepted plan")?;
    if stable_workspace_id(workspace_root)? != accepted.plan.workspace_id {
        bail!("Intent impact preview workspace is out of scope");
    }
    if !accepted
        .plan
        .intents
        .iter()
        .any(|intent| intent.intent_ref == *intent_ref)
    {
        bail!("Intent impact preview target is stale or unknown");
    }
    let target_ids = dependency_closure(&accepted.plan, &intent_ref.intent_id);
    let target_intents = accepted
        .plan
        .intents
        .iter()
        .filter(|intent| target_ids.contains(&intent.intent_ref.intent_id))
        .map(|intent| intent.intent_ref.clone())
        .collect::<Vec<_>>();
    let retained_intents = accepted
        .plan
        .intents
        .iter()
        .filter(|intent| !target_ids.contains(&intent.intent_ref.intent_id))
        .map(|intent| intent.intent_ref.clone())
        .collect::<Vec<_>>();
    let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
    let verification_impacts = target_intents
        .iter()
        .flat_map(|target| lineage.current_system_verification_receipt_ids(target))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|receipt_id| IntentVerificationImpactV1 {
            receipt_id,
            impact: IntentVerificationImpact::BecomesStale,
        })
        .collect::<Vec<_>>();
    let workspace_revision = session
        .mutation_event_recorder()
        .context("Intent impact preview requires mutation workspace evidence")?
        .current_workspace_revision(workspace_root)?;
    let source_frontier = records
        .last()
        .map(|record| record.stored_event().stream_sequence)
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(b"sigil.intent.impact.v1\0");
    hasher.update(accepted.plan.plan_digest.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(intent_ref.intent_id.as_str().as_bytes());
    hasher.update(intent_ref.version.to_le_bytes());
    hasher.update(format!("{operation_kind:?}").as_bytes());
    hasher.update(proposal_digest.unwrap_or("no-proposal").as_bytes());
    hasher.update(workspace_revision.to_le_bytes());
    hasher.update(source_frontier.to_le_bytes());
    let operation_id = IntentOperationId::new(format!("intent-impact-{:x}", hasher.finalize()))?;
    let mut operation = IntentOperationPreviewV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        operation_id,
        operation_kind,
        stack_id: accepted.plan.stack_id.clone(),
        stack_version: accepted.plan.stack_version,
        target_intents: target_intents.clone(),
        target_is_leaf: target_intents.len() == 1,
        workspace_revision,
        expires_at_ms: None,
        file_effects: Vec::new(),
        retained_intents,
        verification_impacts,
        conflicts: Vec::new(),
        preview_digest: zero_digest()?,
    };
    operation.preview_digest = operation.computed_digest()?;
    let application_impacts = target_intents
        .iter()
        .map(|target| IntentApplicationImpactV1 {
            intent_ref: target.clone(),
            application_state: if operation_kind == IntentOperationKind::ReplaceImpactPreview
                || target == intent_ref
            {
                IntentApplicationState::NeedsRebuild
            } else {
                IntentApplicationState::NeedsReview
            },
        })
        .collect();
    let preview = IntentImpactPreviewV1 {
        operation,
        application_impacts,
    };
    preview.validate_contract()?;
    Ok(preview)
}

fn dependency_closure(plan: &IntentPlanV1, target: &IntentId) -> BTreeSet<IntentId> {
    let mut closure = BTreeSet::from([target.clone()]);
    loop {
        let mut changed = false;
        for intent in &plan.intents {
            if !closure.contains(&intent.intent_ref.intent_id)
                && intent
                    .depends_on
                    .iter()
                    .any(|dependency| closure.contains(dependency))
            {
                changed |= closure.insert(intent.intent_ref.intent_id.clone());
            }
        }
        if !changed {
            return closure;
        }
    }
}

fn durable_records(session: &Session) -> Result<Vec<crate::SessionStreamRecord>> {
    let store = session
        .durable_store()
        .context("Intent impact requires a durable session")?;
    JsonlSessionStore::read_event_records(store.path())
}

fn zero_digest() -> Result<IntentDigest> {
    IntentDigest::new(format!(
        "{}{}",
        INTENT_CANONICAL_DIGEST_PREFIX,
        "0".repeat(64)
    ))
}

fn validate_identity(label: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        bail!("{label} is empty, too long, or contains control characters");
    }
    Ok(())
}

fn validate_text(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        bail!("{label} is empty, too long, or contains control characters");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/intent_impact_tests.rs"]
mod tests;
