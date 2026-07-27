use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::{
    ControlEntry, DurableEventType, EventClass, INTENT_CONTRACT_SCHEMA_VERSION,
    INTENT_PUBLIC_DTO_SCHEMA_VERSION, IntentAcceptanceCriterionV1, IntentAcceptanceKind,
    IntentApplicationState, IntentAuthorityState, IntentDefinitionState, IntentDefinitionV1,
    IntentDigest, IntentEventV1, IntentId, IntentPlanKind, IntentPlanProposalV1, IntentPlanV1,
    IntentProposalCriterionV1, IntentSourceV1, IntentStackId, IntentStackVersion,
    IntentTaskPlanBindingV1, IntentVersionRef, JsonlSessionStore, MAX_INTENT_CRITERIA,
    MAX_INTENT_STATEMENT_BYTES, MAX_INTENT_TITLE_BYTES, ProjectionApplyDecision, ProjectionCursor,
    PublicIntentSourceV1, PublicIntentStackStateV1, PublicIntentStackV1, PublicIntentV1, Session,
    SessionLogEntry, SessionStreamRecord, TaskPlanEntry, TaskPlanStatus, TaskStepId, TaskStepMode,
    TypedDomainEvent, TypedStoredEventDecode, decode_typed_stored_event, projection_apply_decision,
    validate_task_plan_graph_steps,
};

/// Projection schema for append-only Intent admission state.
pub const INTENT_ADMISSION_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// Stable adapter text for sessions that predate Intent Stack durable facts.
pub const INTENT_HISTORY_UNAVAILABLE_MESSAGE: &str =
    "Intent history is unavailable for this session.";
/// Host alias for the single automatically admitted user-declared root.
pub const USER_DECLARED_ROOT_INTENT_ALIAS: &str = "root";

/// Host-owned identity and workspace scope for one initial IntentPlan admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentAdmissionContextV1 {
    pub stack_id: IntentStackId,
    pub stack_version: IntentStackVersion,
    pub workspace_id: String,
    pub source_session_id: String,
}

impl IntentAdmissionContextV1 {
    /// Creates the initial stack context supported by R51.1.
    ///
    /// # Errors
    ///
    /// Returns an error when the workspace/session scope is empty or a later stack version is
    /// requested before revise/supersede semantics exist.
    pub fn initial(
        stack_id: IntentStackId,
        workspace_id: impl Into<String>,
        source_session_id: impl Into<String>,
    ) -> Result<Self> {
        let workspace_id = workspace_id.into();
        let source_session_id = source_session_id.into();
        validate_admission_identity("intent admission workspace id", &workspace_id)?;
        validate_admission_identity("intent admission source session id", &source_session_id)?;
        Ok(Self {
            stack_id,
            stack_version: IntentStackVersion::new(1)?,
            workspace_id,
            source_session_id,
        })
    }
}

/// Host-resolved root definition for the original user outcome.
///
/// Criterion aliases are local draft keys only. Runtime admission replaces them with
/// content-bound criterion ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserDeclaredIntentV1 {
    pub title: String,
    pub statement: String,
    pub acceptance_criteria: Vec<IntentProposalCriterionV1>,
}

/// Non-serializable host authority proving that acceptance did not come from provider JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentAcceptanceAuthorityV1 {
    kind: IntentAcceptanceKind,
    source_turn_id: String,
    authority_event_id: String,
    proposal_digest: Option<IntentDigest>,
}

impl IntentAcceptanceAuthorityV1 {
    /// Binds the original user turn to automatic single-root admission.
    ///
    /// # Errors
    ///
    /// Returns an error when either host identity is empty or unbounded.
    pub fn user_declared_root(
        source_turn_id: impl Into<String>,
        authority_event_id: impl Into<String>,
    ) -> Result<Self> {
        Self::new(
            IntentAcceptanceKind::UserDeclaredRootAdmission,
            source_turn_id.into(),
            authority_event_id.into(),
            None,
        )
    }

    /// Binds an explicit user confirmation to one exact provider proposal digest.
    ///
    /// # Errors
    ///
    /// Returns an error when either host identity is empty or unbounded.
    pub fn explicit_user_confirmation(
        source_turn_id: impl Into<String>,
        authority_event_id: impl Into<String>,
        proposal_digest: IntentDigest,
    ) -> Result<Self> {
        Self::new(
            IntentAcceptanceKind::ExplicitUserConfirmation,
            source_turn_id.into(),
            authority_event_id.into(),
            Some(proposal_digest),
        )
    }

    fn new(
        kind: IntentAcceptanceKind,
        source_turn_id: String,
        authority_event_id: String,
        proposal_digest: Option<IntentDigest>,
    ) -> Result<Self> {
        validate_admission_identity("intent acceptance source turn id", &source_turn_id)?;
        validate_admission_identity("intent acceptance authority event id", &authority_event_id)?;
        Ok(Self {
            kind,
            source_turn_id,
            authority_event_id,
            proposal_digest,
        })
    }
}

/// Fully resolved, immutable IntentPlan plus its independent acceptance authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentPlanAdmissionV1 {
    plan: IntentPlanV1,
    acceptance_kind: IntentAcceptanceKind,
    source_turn_id: String,
    acceptance_authority_id: String,
    resolved_aliases: BTreeMap<String, IntentVersionRef>,
}

impl IntentPlanAdmissionV1 {
    #[must_use]
    pub fn plan(&self) -> &IntentPlanV1 {
        &self.plan
    }

    #[must_use]
    pub fn acceptance_kind(&self) -> IntentAcceptanceKind {
        self.acceptance_kind
    }

    /// Returns the runtime-owned accepted intent ref resolved from one provider-local alias.
    #[must_use]
    pub fn intent_ref_for_alias(&self, alias: &str) -> Option<&IntentVersionRef> {
        self.resolved_aliases.get(alias)
    }

    fn stack_created_event(&self) -> IntentEventV1 {
        IntentEventV1::StackCreated {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            stack_id: self.plan.stack_id.clone(),
            workspace_id: self.plan.workspace_id.clone(),
            source_session_id: self.plan.source_session_id.clone(),
        }
    }

    fn plan_recorded_event(&self) -> IntentEventV1 {
        IntentEventV1::PlanRecorded {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            plan: self.plan.clone(),
        }
    }

    fn plan_accepted_event(
        &self,
        task_plan_binding: Option<IntentTaskPlanBindingV1>,
    ) -> IntentEventV1 {
        IntentEventV1::PlanAccepted {
            schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
            stack_id: self.plan.stack_id.clone(),
            stack_version: self.plan.stack_version,
            plan_digest: self.plan.plan_digest.clone(),
            acceptance_kind: self.acceptance_kind,
            source_turn_id: self.source_turn_id.clone(),
            acceptance_authority_id: self.acceptance_authority_id.clone(),
            task_plan_binding,
        }
    }

    fn durable_events(
        &self,
        task_plan_binding: Option<IntentTaskPlanBindingV1>,
    ) -> Result<Vec<(DurableEventType, EventClass, serde_json::Value)>> {
        let events = [
            (
                DurableEventType::IntentStackCreated,
                self.stack_created_event(),
            ),
            (
                DurableEventType::IntentPlanRecorded,
                self.plan_recorded_event(),
            ),
            (
                DurableEventType::IntentPlanAccepted,
                self.plan_accepted_event(task_plan_binding),
            ),
        ];
        events
            .into_iter()
            .map(|(event_type, event)| {
                event.validate_contract()?;
                Ok((
                    event_type,
                    EventClass::Critical,
                    serde_json::to_value(event)
                        .context("failed to serialize Intent admission event")?,
                ))
            })
            .collect()
    }
}

/// Host-owned mapping from one Task step to provider-local intent aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskStepIntentAliasBindingV1 {
    pub step_id: TaskStepId,
    pub intent_aliases: Vec<String>,
}

/// Resolves provider-local aliases into accepted stable refs before TaskPlan persistence.
///
/// The returned plan contains no provider aliases. Every write step must resolve exactly one
/// accepted ref; read and review steps may bind a dependency closure or remain unbound.
///
/// # Errors
///
/// Returns an error for an unknown/duplicate alias, an unknown/duplicate step mapping, or a write
/// step that does not bind exactly one accepted intent.
pub fn bind_task_plan_intents(
    admission: &IntentPlanAdmissionV1,
    mut task_plan: TaskPlanEntry,
    bindings: &[TaskStepIntentAliasBindingV1],
) -> Result<TaskPlanEntry> {
    validate_task_plan_graph_steps(&task_plan.steps)?;
    let mut by_step = BTreeMap::<TaskStepId, Vec<IntentVersionRef>>::new();
    for binding in bindings {
        if by_step.contains_key(&binding.step_id) {
            bail!(
                "task step {} has more than one intent alias mapping",
                binding.step_id.as_str()
            );
        }
        let mut aliases = BTreeSet::new();
        let mut refs = Vec::with_capacity(binding.intent_aliases.len());
        for alias in &binding.intent_aliases {
            if !aliases.insert(alias.as_str()) {
                bail!(
                    "task step {} repeats intent alias {alias}",
                    binding.step_id.as_str()
                );
            }
            refs.push(
                admission
                    .intent_ref_for_alias(alias)
                    .cloned()
                    .with_context(|| {
                        format!("task step references unknown intent alias {alias}")
                    })?,
            );
        }
        if refs.iter().collect::<BTreeSet<_>>().len() != refs.len() {
            bail!(
                "task step {} aliases resolve to duplicate intent refs",
                binding.step_id.as_str()
            );
        }
        by_step.insert(binding.step_id.clone(), refs);
    }
    for step in &mut task_plan.steps {
        step.intent_refs = by_step.remove(&step.step_id).unwrap_or_default();
        if step.effective_mode() == TaskStepMode::Write && step.intent_refs.len() != 1 {
            bail!(
                "Intent-enabled write task step {} must bind exactly one accepted intent",
                step.step_id.as_str()
            );
        }
    }
    if let Some((step_id, _)) = by_step.first_key_value() {
        bail!(
            "intent alias mapping references unknown task step {}",
            step_id.as_str()
        );
    }
    validate_task_plan_intent_refs(admission.plan(), &task_plan)?;
    Ok(task_plan)
}

/// Resolves one user-declared root into runtime-owned ids and a digest-bound plan.
///
/// # Errors
///
/// Returns an error when the authority is not the original user-turn admission, the root is
/// malformed, or the resulting plan fails the locked R51 contract.
pub fn admit_user_declared_root(
    context: &IntentAdmissionContextV1,
    root: UserDeclaredIntentV1,
    authority: &IntentAcceptanceAuthorityV1,
) -> Result<IntentPlanAdmissionV1> {
    if context.stack_version.get() != 1 {
        bail!("R51.1 only admits the initial IntentPlan version");
    }
    if authority.kind != IntentAcceptanceKind::UserDeclaredRootAdmission
        || authority.proposal_digest.is_some()
    {
        bail!("user-declared root admission requires original user-turn authority");
    }
    validate_root_draft(&root)?;
    let intent_id = runtime_intent_id(context, "root", &authority.source_turn_id)?;
    let acceptance_criteria = root
        .acceptance_criteria
        .into_iter()
        .map(|criterion| {
            let criterion_id =
                runtime_criterion_id(context, intent_id.as_str(), &criterion.criterion_alias)?;
            Ok(IntentAcceptanceCriterionV1 {
                criterion_id,
                statement: criterion.statement,
                required: criterion.required,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let definition = IntentDefinitionV1 {
        intent_ref: IntentVersionRef::new(intent_id, 1)?,
        title: root.title,
        statement: root.statement,
        acceptance_criteria,
        depends_on: Vec::new(),
        source: IntentSourceV1::UserTurn {
            source_turn_id: authority.source_turn_id.clone(),
        },
        supersedes: None,
    };
    let resolved_aliases = BTreeMap::from([(
        USER_DECLARED_ROOT_INTENT_ALIAS.to_owned(),
        definition.intent_ref.clone(),
    )]);
    build_admission(
        context,
        IntentPlanKind::UserDeclaredRoot,
        vec![definition],
        resolved_aliases,
        authority,
    )
}

/// Converts an untrusted provider proposal into a runtime-owned accepted-plan candidate.
///
/// The provider aliases are never reused as runtime ids and this function requires a separate
/// host authority bound to the exact proposal digest.
///
/// # Errors
///
/// Returns an error when the proposal is malformed, its source/digest does not match the explicit
/// user confirmation, or runtime id resolution produces an invalid plan.
pub fn admit_suggested_decomposition(
    context: &IntentAdmissionContextV1,
    proposal: &IntentPlanProposalV1,
    authority: &IntentAcceptanceAuthorityV1,
) -> Result<IntentPlanAdmissionV1> {
    if context.stack_version.get() != 1 {
        bail!("R51.1 only admits the initial IntentPlan version");
    }
    proposal.validate_contract()?;
    if authority.kind != IntentAcceptanceKind::ExplicitUserConfirmation
        || authority.proposal_digest.as_ref() != Some(&proposal.proposal_digest)
        || authority.source_turn_id != proposal.source_turn_id
    {
        bail!("suggested decomposition requires explicit authority for the exact proposal");
    }
    let mut ids = BTreeMap::new();
    for proposed in &proposal.intents {
        ids.insert(
            proposed.intent_alias.as_str(),
            runtime_intent_id(context, "suggested", &proposed.intent_alias)?,
        );
    }
    let intents = proposal
        .intents
        .iter()
        .map(|proposed| {
            let intent_id = ids
                .get(proposed.intent_alias.as_str())
                .context("intent proposal alias disappeared during runtime resolution")?
                .clone();
            let acceptance_criteria = proposed
                .acceptance_criteria
                .iter()
                .map(|criterion| {
                    Ok(IntentAcceptanceCriterionV1 {
                        criterion_id: runtime_criterion_id(
                            context,
                            intent_id.as_str(),
                            &criterion.criterion_alias,
                        )?,
                        statement: criterion.statement.clone(),
                        required: criterion.required,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let depends_on = proposed
                .depends_on_aliases
                .iter()
                .map(|alias| {
                    ids.get(alias.as_str())
                        .cloned()
                        .context("intent dependency alias disappeared during runtime resolution")
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(IntentDefinitionV1 {
                intent_ref: IntentVersionRef::new(intent_id, 1)?,
                title: proposed.title.clone(),
                statement: proposed.statement.clone(),
                acceptance_criteria,
                depends_on,
                source: IntentSourceV1::AcceptedSuggestion {
                    source_turn_id: proposal.source_turn_id.clone(),
                    proposal_digest: proposal.proposal_digest.clone(),
                },
                supersedes: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let resolved_aliases = ids
        .into_iter()
        .map(|(alias, intent_id)| Ok((alias.to_owned(), IntentVersionRef::new(intent_id, 1)?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    build_admission(
        context,
        IntentPlanKind::SuggestedDecomposition,
        intents,
        resolved_aliases,
        authority,
    )
}

fn build_admission(
    context: &IntentAdmissionContextV1,
    kind: IntentPlanKind,
    intents: Vec<IntentDefinitionV1>,
    resolved_aliases: BTreeMap<String, IntentVersionRef>,
    authority: &IntentAcceptanceAuthorityV1,
) -> Result<IntentPlanAdmissionV1> {
    let mut plan = IntentPlanV1 {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        stack_id: context.stack_id.clone(),
        stack_version: context.stack_version,
        workspace_id: context.workspace_id.clone(),
        source_session_id: context.source_session_id.clone(),
        kind,
        intents,
        plan_digest: empty_intent_digest()?,
    };
    plan.plan_digest = plan.computed_digest()?;
    plan.validate_contract()?;
    Ok(IntentPlanAdmissionV1 {
        plan,
        acceptance_kind: authority.kind,
        source_turn_id: authority.source_turn_id.clone(),
        acceptance_authority_id: authority.authority_event_id.clone(),
        resolved_aliases,
    })
}

/// One accepted plan reconstructed from durable admission events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedIntentPlanProjectionV1 {
    pub plan: IntentPlanV1,
    pub acceptance_kind: IntentAcceptanceKind,
    pub source_turn_id: String,
    pub acceptance_authority_id: String,
    pub task_plan_binding: Option<IntentTaskPlanBindingV1>,
    pub accepted_event_id: String,
    pub accepted_stream_sequence: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IntentStackHeaderV1 {
    stack_id: IntentStackId,
    workspace_id: String,
    source_session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PendingTaskIntentAcceptanceV1 {
    accepted: AcceptedIntentPlanProjectionV1,
}

/// Append-only IntentPlan admission projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentStackProjectionV1 {
    cursor: Option<ProjectionCursor>,
    header: Option<IntentStackHeaderV1>,
    recorded_plans: BTreeMap<u64, IntentPlanV1>,
    accepted_plans: BTreeMap<u64, AcceptedIntentPlanProjectionV1>,
    pending_task_acceptance: Option<PendingTaskIntentAcceptanceV1>,
}

impl IntentStackProjectionV1 {
    /// Replays the full session stream and fails closed on invalid or incomplete ordering.
    ///
    /// A crash after `IntentPlanAccepted` but before its immediately following TaskPlan record is
    /// retained as an incomplete admission. It does not become the latest accepted plan.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut projection = Self::default();
        for record in records {
            projection.apply_record(record)?;
        }
        Ok(projection)
    }

    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }

    #[must_use]
    pub fn latest_accepted_plan(&self) -> Option<&AcceptedIntentPlanProjectionV1> {
        self.accepted_plans.last_key_value().map(|(_, plan)| plan)
    }

    #[must_use]
    pub fn has_incomplete_task_acceptance(&self) -> bool {
        self.pending_task_acceptance.is_some()
    }

    /// Produces the bounded adapter contract without guessing legacy history or incomplete state.
    ///
    /// # Errors
    ///
    /// Returns an error when a stack exists but its latest admission is incomplete.
    pub fn public_state(&self) -> Result<PublicIntentStackStateV1> {
        self.public_state_with_optional_lineage(None)
    }

    /// Produces the bounded adapter contract with R51.2 lineage and evidence summaries.
    pub fn public_state_with_lineage(
        &self,
        lineage: &crate::IntentLineageProjectionV1,
    ) -> Result<PublicIntentStackStateV1> {
        self.public_state_with_optional_lineage(Some(lineage))
    }

    fn public_state_with_optional_lineage(
        &self,
        lineage: Option<&crate::IntentLineageProjectionV1>,
    ) -> Result<PublicIntentStackStateV1> {
        let Some(header) = &self.header else {
            return Ok(PublicIntentStackStateV1::HistoryUnavailable {
                schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
                safe_message: INTENT_HISTORY_UNAVAILABLE_MESSAGE.to_owned(),
            });
        };
        if self.pending_task_acceptance.is_some()
            || self
                .recorded_plans
                .last_key_value()
                .map(|(version, _)| Some(*version))
                != self
                    .accepted_plans
                    .last_key_value()
                    .map(|(version, _)| Some(*version))
        {
            bail!("IntentPlan admission is incomplete and cannot be rendered as accepted");
        }
        let accepted = self
            .latest_accepted_plan()
            .context("Intent Stack has no accepted plan")?;
        let intents = accepted
            .plan
            .intents
            .iter()
            .map(|definition| {
                public_intent_from_definition(
                    definition,
                    lineage.map(|lineage| lineage.summary_for(&definition.intent_ref)),
                )
            })
            .collect::<Vec<_>>();
        let authority_state = if intents
            .iter()
            .any(|intent| intent.application_state == IntentApplicationState::ReadOnly)
        {
            IntentAuthorityState::ReadOnlyProvenance
        } else {
            IntentAuthorityState::Active
        };
        Ok(PublicIntentStackStateV1::Available {
            schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
            stack: PublicIntentStackV1 {
                schema_version: INTENT_PUBLIC_DTO_SCHEMA_VERSION,
                stack_id: header.stack_id.clone(),
                stack_version: accepted.plan.stack_version,
                authority_state,
                plan_digest: accepted.plan.plan_digest.clone(),
                intents,
                conflicts: Vec::new(),
            },
        })
    }

    fn apply_record(&mut self, record: &SessionStreamRecord) -> Result<()> {
        let event = record.stored_event();
        if projection_apply_decision(self.cursor.as_ref(), event)?
            == ProjectionApplyDecision::IgnoreAlreadyApplied
        {
            return Ok(());
        }
        let typed = match decode_typed_stored_event(event.clone())? {
            TypedStoredEventDecode::Known(event) => *event,
            TypedStoredEventDecode::UnknownNonCritical(_) => {
                if self.pending_task_acceptance.is_some() {
                    bail!(
                        "task-bound IntentPlan acceptance was not followed by its TaskPlan record"
                    );
                }
                self.advance_cursor(record);
                return Ok(());
            }
        };

        if let Some(pending) = self.pending_task_acceptance.take() {
            match typed {
                TypedDomainEvent::TaskStatusChanged(ControlEntry::TaskPlan(task_plan)) => {
                    validate_pending_task_plan(&pending, &task_plan, event.stream_sequence)?;
                    let version = pending.accepted.plan.stack_version.get();
                    self.accepted_plans.insert(version, pending.accepted);
                    self.advance_cursor(record);
                    return Ok(());
                }
                _ => {
                    self.pending_task_acceptance = Some(pending);
                    bail!(
                        "task-bound IntentPlan acceptance was not followed by its TaskPlan record"
                    );
                }
            }
        }

        if let TypedDomainEvent::Intent(intent_event) = typed {
            self.apply_intent_event(event, intent_event)?;
        }
        self.advance_cursor(record);
        Ok(())
    }

    fn apply_intent_event(
        &mut self,
        event: &crate::StoredEvent,
        intent_event: IntentEventV1,
    ) -> Result<()> {
        intent_event.validate_contract()?;
        match intent_event {
            IntentEventV1::StackCreated {
                stack_id,
                workspace_id,
                source_session_id,
                ..
            } => {
                if self.header.is_some() {
                    bail!("Intent Stack was created more than once");
                }
                if source_session_id != event.session_id {
                    bail!("Intent Stack source session does not match its durable stream");
                }
                self.header = Some(IntentStackHeaderV1 {
                    stack_id,
                    workspace_id,
                    source_session_id,
                });
            }
            IntentEventV1::PlanRecorded { plan, .. } => {
                let header = self
                    .header
                    .as_ref()
                    .context("IntentPlan was recorded before Intent Stack creation")?;
                validate_plan_header(header, &plan, &event.session_id)?;
                let expected_version = self
                    .recorded_plans
                    .last_key_value()
                    .map_or(1, |(version, _)| version.saturating_add(1));
                let version = plan.stack_version.get();
                if version != expected_version {
                    bail!(
                        "IntentPlan version {version} does not follow recorded version {}",
                        expected_version.saturating_sub(1)
                    );
                }
                if self.recorded_plans.insert(version, plan).is_some() {
                    bail!("IntentPlan version was recorded more than once");
                }
            }
            IntentEventV1::PlanAccepted {
                stack_id,
                stack_version,
                plan_digest,
                acceptance_kind,
                source_turn_id,
                acceptance_authority_id,
                task_plan_binding,
                ..
            } => {
                let header = self
                    .header
                    .as_ref()
                    .context("IntentPlan was accepted before Intent Stack creation")?;
                if header.stack_id != stack_id {
                    bail!("IntentPlan acceptance references another stack");
                }
                let version = stack_version.get();
                let plan = self
                    .recorded_plans
                    .get(&version)
                    .context("IntentPlan acceptance has no recorded plan")?;
                if plan.plan_digest != plan_digest {
                    bail!("IntentPlan acceptance digest does not match the recorded plan");
                }
                if self.accepted_plans.contains_key(&version) {
                    bail!("IntentPlan version was accepted more than once");
                }
                let expected_version = self
                    .accepted_plans
                    .last_key_value()
                    .map_or(1, |(accepted_version, _)| {
                        accepted_version.saturating_add(1)
                    });
                if version != expected_version {
                    bail!("IntentPlan acceptance is not monotonic");
                }
                validate_acceptance_for_plan(
                    plan,
                    acceptance_kind,
                    &source_turn_id,
                    task_plan_binding.as_ref(),
                )?;
                let accepted = AcceptedIntentPlanProjectionV1 {
                    plan: plan.clone(),
                    acceptance_kind,
                    source_turn_id,
                    acceptance_authority_id,
                    task_plan_binding: task_plan_binding.clone(),
                    accepted_event_id: event.event_id.clone(),
                    accepted_stream_sequence: event.stream_sequence,
                };
                if task_plan_binding.is_some() {
                    self.pending_task_acceptance = Some(PendingTaskIntentAcceptanceV1 { accepted });
                } else {
                    self.accepted_plans.insert(version, accepted);
                }
            }
            IntentEventV1::ExecutionBound { .. }
            | IntentEventV1::ChangeSetBound { .. }
            | IntentEventV1::VerificationLinked { .. } => {
                // R51.2 lineage is reduced by `IntentLineageProjectionV1`; admission remains an
                // immutable view of accepted plan versions.
            }
            _ => bail!(
                "{} is registered by a later RFC-0051 slice",
                intent_event.event_type()
            ),
        }
        Ok(())
    }

    fn advance_cursor(&mut self, record: &SessionStreamRecord) {
        self.cursor = Some(record.projection_cursor(INTENT_ADMISSION_PROJECTION_SCHEMA_VERSION));
    }

    fn validate_initial_append(&self, admission: &IntentPlanAdmissionV1) -> Result<bool> {
        if self.header.is_none()
            && self.recorded_plans.is_empty()
            && self.accepted_plans.is_empty()
            && self.pending_task_acceptance.is_none()
        {
            if admission.plan.stack_version.get() != 1 {
                bail!("initial IntentPlan admission must use stack version one");
            }
            return Ok(true);
        }
        if self.pending_task_acceptance.is_some() {
            bail!("an incomplete task-bound IntentPlan admission requires recovery");
        }
        bail!("R51.1 does not admit a second semantic IntentPlan version")
    }

    fn is_exact_task_admission(
        &self,
        admission: &IntentPlanAdmissionV1,
        binding: &IntentTaskPlanBindingV1,
    ) -> bool {
        self.latest_accepted_plan().is_some_and(|accepted| {
            accepted.plan == admission.plan
                && accepted.acceptance_kind == admission.acceptance_kind
                && accepted.source_turn_id == admission.source_turn_id
                && accepted.acceptance_authority_id == admission.acceptance_authority_id
                && accepted.task_plan_binding.as_ref() == Some(binding)
        })
    }

    fn is_exact_chat_admission(&self, admission: &IntentPlanAdmissionV1) -> bool {
        self.latest_accepted_plan().is_some_and(|accepted| {
            accepted.plan == admission.plan
                && accepted.acceptance_kind == admission.acceptance_kind
                && accepted.source_turn_id == admission.source_turn_id
                && accepted.acceptance_authority_id == admission.acceptance_authority_id
                && accepted.task_plan_binding.is_none()
        })
    }
}

/// Result of one idempotent durable admission attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentAdmissionWriteOutcomeV1 {
    pub appended: bool,
    pub stack_id: IntentStackId,
    pub stack_version: IntentStackVersion,
    pub plan_digest: IntentDigest,
}

/// Appends stack creation, plan recording, independent acceptance, and the accepted TaskPlan in
/// one ordered writer batch.
///
/// The TaskPlan record is deliberately last. A crash after any Intent prefix cannot make a write
/// participant runnable because the accepted TaskPlan is still absent.
///
/// # Errors
///
/// Returns an error for an in-memory session, a scope mismatch, a non-accepted/invalid TaskPlan,
/// conflicting prior admission state, or any durable write failure.
pub fn append_task_intent_plan_admission(
    session: &mut Session,
    admission: &IntentPlanAdmissionV1,
    task_plan: TaskPlanEntry,
) -> Result<IntentAdmissionWriteOutcomeV1> {
    ensure_admission_matches_session(session, admission)?;
    if task_plan.status != TaskPlanStatus::Accepted {
        bail!("IntentPlan task admission requires an accepted TaskPlan");
    }
    validate_task_plan_graph_steps(&task_plan.steps)?;
    validate_task_plan_intent_refs(admission.plan(), &task_plan)?;
    let binding = IntentTaskPlanBindingV1 {
        task_id: task_plan.task_id.as_str().to_owned(),
        task_plan_version: task_plan.plan_version,
    };
    let durable_events = admission.durable_events(Some(binding.clone()))?;
    let task_entry = SessionLogEntry::Control(ControlEntry::TaskPlan(task_plan.clone()));
    let store = session
        .durable_store()
        .context("IntentPlan admission requires a durable session store")?;
    let predicate_admission = admission.clone();
    let predicate_binding = binding.clone();
    let predicate_task_plan = task_plan.clone();
    let appended = store
        .append_events_and_session_entries_if(
            durable_events,
            std::slice::from_ref(&task_entry),
            move |records| {
                let projection = IntentStackProjectionV1::from_records(records)?;
                if projection.is_exact_task_admission(&predicate_admission, &predicate_binding) {
                    let existing = durable_task_plan(records, &predicate_binding)?
                        .context("accepted IntentPlan is missing its durable TaskPlan")?;
                    if existing != predicate_task_plan {
                        bail!("durable TaskPlan conflicts with the accepted IntentPlan binding");
                    }
                    return Ok(false);
                }
                if durable_task_plan(records, &predicate_binding)?.is_some() {
                    bail!("TaskPlan version already exists without the requested IntentPlan");
                }
                projection.validate_initial_append(&predicate_admission)
            },
        )?
        .is_some();
    if appended || !live_session_has_task_plan(session, &task_plan) {
        session.record_durably_appended_control(ControlEntry::TaskPlan(task_plan));
    }
    Ok(IntentAdmissionWriteOutcomeV1 {
        appended,
        stack_id: admission.plan.stack_id.clone(),
        stack_version: admission.plan.stack_version,
        plan_digest: admission.plan.plan_digest.clone(),
    })
}

/// Appends a user-declared Chat root before any later mutating-tool admission.
///
/// R51.1 records acceptance only. R51.2 binds the root logical run and first concrete attempt.
///
/// # Errors
///
/// Returns an error for suggested decomposition, a task-bound authority, a scope mismatch,
/// conflicting prior state, or any durable write failure.
pub fn append_chat_root_intent_admission(
    session: &Session,
    admission: &IntentPlanAdmissionV1,
) -> Result<IntentAdmissionWriteOutcomeV1> {
    ensure_admission_matches_session(session, admission)?;
    if admission.plan.kind != IntentPlanKind::UserDeclaredRoot
        || admission.acceptance_kind != IntentAcceptanceKind::UserDeclaredRootAdmission
    {
        bail!("Chat admission only accepts the original user-declared root");
    }
    let durable_events = admission.durable_events(None)?;
    let store = session
        .durable_store()
        .context("IntentPlan admission requires a durable session store")?;
    let predicate_admission = admission.clone();
    let appended = store
        .append_events_and_session_entries_if(durable_events, &[], move |records| {
            let projection = IntentStackProjectionV1::from_records(records)?;
            if projection.is_exact_chat_admission(&predicate_admission) {
                return Ok(false);
            }
            projection.validate_initial_append(&predicate_admission)
        })?
        .is_some();
    Ok(IntentAdmissionWriteOutcomeV1 {
        appended,
        stack_id: admission.plan.stack_id.clone(),
        stack_version: admission.plan.stack_version,
        plan_digest: admission.plan.plan_digest.clone(),
    })
}

impl Session {
    /// Rebuilds Intent admission state from the durable stream.
    ///
    /// In-memory/legacy sessions have no authoritative intent history and therefore return an
    /// empty projection whose public state is `history_unavailable`.
    pub fn intent_stack_projection(&self) -> Result<IntentStackProjectionV1> {
        let Some(store) = self.durable_store() else {
            return Ok(IntentStackProjectionV1::default());
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        IntentStackProjectionV1::from_records(&records)
    }

    /// Returns the bounded Intent Stack adapter DTO.
    pub fn public_intent_stack_state(&self) -> Result<PublicIntentStackStateV1> {
        let Some(store) = self.durable_store() else {
            return IntentStackProjectionV1::default().public_state();
        };
        let records = JsonlSessionStore::read_event_records(store.path())?;
        let admission = IntentStackProjectionV1::from_records(&records)?;
        if admission.latest_accepted_plan().is_none() {
            return admission.public_state();
        }
        let lineage = crate::IntentLineageProjectionV1::from_records(&records, &admission)?;
        admission.public_state_with_lineage(&lineage)
    }
}

fn validate_pending_task_plan(
    pending: &PendingTaskIntentAcceptanceV1,
    task_plan: &TaskPlanEntry,
    stream_sequence: u64,
) -> Result<()> {
    let binding = pending
        .accepted
        .task_plan_binding
        .as_ref()
        .context("pending task acceptance is missing its TaskPlan binding")?;
    if stream_sequence != pending.accepted.accepted_stream_sequence.saturating_add(1) {
        bail!("task-bound IntentPlan and TaskPlan are not adjacent in the durable writer batch");
    }
    if task_plan.status != TaskPlanStatus::Accepted
        || task_plan.task_id.as_str() != binding.task_id
        || task_plan.plan_version != binding.task_plan_version
    {
        bail!("task-bound IntentPlan acceptance does not match the following TaskPlan");
    }
    validate_task_plan_graph_steps(&task_plan.steps)
}

fn validate_plan_header(
    header: &IntentStackHeaderV1,
    plan: &IntentPlanV1,
    session_id: &str,
) -> Result<()> {
    plan.validate_contract()?;
    if plan.stack_id != header.stack_id
        || plan.workspace_id != header.workspace_id
        || plan.source_session_id != header.source_session_id
        || plan.source_session_id != session_id
    {
        bail!("IntentPlan scope does not match its stack header and durable session");
    }
    Ok(())
}

fn validate_task_plan_intent_refs(plan: &IntentPlanV1, task_plan: &TaskPlanEntry) -> Result<()> {
    let accepted_refs = plan
        .intents
        .iter()
        .map(|intent| &intent.intent_ref)
        .collect::<BTreeSet<_>>();
    for step in &task_plan.steps {
        if step
            .intent_refs
            .iter()
            .any(|intent_ref| !accepted_refs.contains(intent_ref))
        {
            bail!(
                "task step {} references an intent outside the accepted IntentPlan",
                step.step_id.as_str()
            );
        }
        if step.effective_mode() == TaskStepMode::Write && step.intent_refs.len() != 1 {
            bail!(
                "Intent-enabled write task step {} must bind exactly one accepted intent",
                step.step_id.as_str()
            );
        }
    }
    Ok(())
}

fn validate_acceptance_for_plan(
    plan: &IntentPlanV1,
    acceptance_kind: IntentAcceptanceKind,
    source_turn_id: &str,
    task_plan_binding: Option<&IntentTaskPlanBindingV1>,
) -> Result<()> {
    validate_admission_identity("intent acceptance source turn id", source_turn_id)?;
    match (plan.kind, acceptance_kind) {
        (IntentPlanKind::UserDeclaredRoot, IntentAcceptanceKind::UserDeclaredRootAdmission) => {
            let matches_source = plan.intents.iter().all(|intent| {
                matches!(
                    &intent.source,
                    IntentSourceV1::UserTurn {
                        source_turn_id: source
                    } if source == source_turn_id
                )
            });
            if !matches_source {
                bail!("user-declared root acceptance does not match its source turn");
            }
        }
        (
            IntentPlanKind::SuggestedDecomposition,
            IntentAcceptanceKind::ExplicitUserConfirmation,
        ) => {
            if task_plan_binding.is_none() {
                bail!("accepted suggested decomposition must be bound to a TaskPlan");
            }
            let mut proposal_digests = BTreeSet::new();
            for intent in &plan.intents {
                let IntentSourceV1::AcceptedSuggestion {
                    source_turn_id: source,
                    proposal_digest,
                } = &intent.source
                else {
                    bail!("suggested decomposition has non-suggestion provenance");
                };
                if source != source_turn_id {
                    bail!("suggested decomposition acceptance source turn does not match");
                }
                proposal_digests.insert(proposal_digest);
            }
            if proposal_digests.len() != 1 {
                bail!("suggested decomposition must bind one exact proposal digest");
            }
        }
        (
            IntentPlanKind::SuggestedDecomposition,
            IntentAcceptanceKind::ContentBoundSpecDecision,
        ) => {
            if task_plan_binding.is_none()
                || plan
                    .intents
                    .iter()
                    .any(|intent| !matches!(intent.source, IntentSourceV1::TrustedSpec { .. }))
            {
                bail!("content-bound spec admission requires trusted-spec Task provenance");
            }
        }
        _ => bail!("IntentPlan kind and acceptance authority are incompatible"),
    }
    Ok(())
}

fn public_intent_from_definition(
    definition: &IntentDefinitionV1,
    lineage: Option<crate::IntentLineageSummaryV1>,
) -> PublicIntentV1 {
    let source = match &definition.source {
        IntentSourceV1::UserTurn { source_turn_id } => PublicIntentSourceV1::UserTurn {
            source_turn_id: source_turn_id.clone(),
        },
        IntentSourceV1::AcceptedSuggestion { source_turn_id, .. } => {
            PublicIntentSourceV1::AcceptedSuggestion {
                source_turn_id: source_turn_id.clone(),
            }
        }
        IntentSourceV1::TrustedSpec { .. } => PublicIntentSourceV1::TrustedSpec {
            safe_source_label: "Trusted specification".to_owned(),
        },
    };
    let lineage = lineage.unwrap_or(crate::IntentLineageSummaryV1 {
        application_state: Some(IntentApplicationState::Unapplied),
        ..crate::IntentLineageSummaryV1::default()
    });
    PublicIntentV1 {
        intent_ref: definition.intent_ref.clone(),
        title: definition.title.clone(),
        statement: definition.statement.clone(),
        acceptance_criteria: definition.acceptance_criteria.clone(),
        depends_on: definition.depends_on.clone(),
        source,
        definition_state: IntentDefinitionState::Accepted,
        application_state: lineage
            .application_state
            .unwrap_or(IntentApplicationState::Unapplied),
        exclusive_artifact_count: 0,
        shared_artifact_count: 0,
        unowned_artifact_count: 0,
        drifted_artifact_count: 0,
        unavailable_artifact_count: 0,
        advisory_criterion_count: lineage.advisory_criterion_count,
        system_verified_criterion_count: lineage.system_verified_criterion_count,
        artifacts: Vec::new(),
        available_actions: Vec::new(),
    }
}

fn durable_task_plan(
    records: &[SessionStreamRecord],
    binding: &IntentTaskPlanBindingV1,
) -> Result<Option<TaskPlanEntry>> {
    let mut matching = None;
    for record in records {
        let Some(SessionLogEntry::Control(ControlEntry::TaskPlan(task_plan))) =
            record.session_log_entry()?
        else {
            continue;
        };
        if task_plan.task_id.as_str() != binding.task_id
            || task_plan.plan_version != binding.task_plan_version
        {
            continue;
        }
        if matching
            .as_ref()
            .is_some_and(|existing: &TaskPlanEntry| existing != &task_plan)
        {
            bail!("durable stream contains conflicting TaskPlan entries for one version");
        }
        matching = Some(task_plan);
    }
    Ok(matching)
}

fn live_session_has_task_plan(session: &Session, expected: &TaskPlanEntry) -> bool {
    session
        .task_state_projection()
        .tasks
        .get(&expected.task_id)
        .and_then(|task| task.plans.get(&expected.plan_version))
        .is_some_and(|plan| {
            plan.status == expected.status
                && plan.steps == expected.steps
                && plan.reason == expected.reason
        })
}

fn ensure_admission_matches_session(
    session: &Session,
    admission: &IntentPlanAdmissionV1,
) -> Result<()> {
    admission.plan.validate_contract()?;
    if admission.plan.source_session_id != session.session_scope_id() {
        bail!("IntentPlan admission belongs to another durable session");
    }
    Ok(())
}

fn validate_root_draft(root: &UserDeclaredIntentV1) -> Result<()> {
    validate_admission_text("intent root title", &root.title, MAX_INTENT_TITLE_BYTES)?;
    validate_admission_text(
        "intent root statement",
        &root.statement,
        MAX_INTENT_STATEMENT_BYTES,
    )?;
    if root.acceptance_criteria.is_empty() {
        bail!("user-declared root requires at least one acceptance criterion");
    }
    if root.acceptance_criteria.len() > MAX_INTENT_CRITERIA {
        bail!("user-declared root contains too many acceptance criteria");
    }
    let mut aliases = BTreeSet::new();
    for criterion in &root.acceptance_criteria {
        validate_runtime_alias("intent root criterion alias", &criterion.criterion_alias)?;
        validate_admission_text(
            "intent root criterion",
            &criterion.statement,
            MAX_INTENT_STATEMENT_BYTES,
        )?;
        if !aliases.insert(criterion.criterion_alias.as_str()) {
            bail!("user-declared root contains duplicate criterion aliases");
        }
    }
    Ok(())
}

fn runtime_intent_id(
    context: &IntentAdmissionContextV1,
    namespace: &str,
    local_key: &str,
) -> Result<IntentId> {
    IntentId::new(runtime_id("intent", context, namespace, local_key))
}

fn runtime_criterion_id(
    context: &IntentAdmissionContextV1,
    intent_id: &str,
    local_key: &str,
) -> Result<crate::IntentCriterionId> {
    crate::IntentCriterionId::new(runtime_id("criterion", context, intent_id, local_key))
}

fn runtime_id(
    prefix: &str,
    context: &IntentAdmissionContextV1,
    namespace: &str,
    local_key: &str,
) -> String {
    let digest = Sha256::digest(
        format!(
            "sigil.intent.admission.v1\0{}\0{}\0{}\0{}\0{}",
            context.stack_id.as_str(),
            context.stack_version.get(),
            context.source_session_id,
            namespace,
            local_key
        )
        .as_bytes(),
    );
    format!("{prefix}-{digest:x}")
}

fn empty_intent_digest() -> Result<IntentDigest> {
    IntentDigest::new(format!(
        "{}{}",
        crate::INTENT_CANONICAL_DIGEST_PREFIX,
        "0".repeat(64)
    ))
}

fn validate_runtime_alias(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        bail!("{label} is not a stable local alias");
    }
    Ok(())
}

fn validate_admission_identity(label: &str, value: &str) -> Result<()> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 256 || trimmed.chars().any(char::is_control) {
        bail!("{label} is invalid");
    }
    Ok(())
}

fn validate_admission_text(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.trim().is_empty()
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        bail!("{label} is invalid");
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/intent_admission_tests.rs"]
mod tests;
