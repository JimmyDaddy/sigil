use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk, ChangeSetValidation,
    ChangeSetValidationKind, ChangeSetValidationStatus, ControlEntry, DurableEventType, EventClass,
    INTENT_CONTRACT_SCHEMA_VERSION, IntegrationPlanRecorded, IntegrationPromotionEffect,
    IntegrationPromotionRecorded, IntegrationPromotionStatus, IntegrationPromotionTarget,
    IntentApplicationState, IntentCriterionEvidenceLevel, IntentCriterionEvidenceV1, IntentEventV1,
    IntentExecutionBindingKind, IntentExecutionBindingV1, IntentExecutionId,
    IntentExecutionOriginV1, IntentStackProjectionV1, IntentVersionRef, JsonlSessionStore,
    MutationBatchFinished, MutationBatchStarted, MutationBatchStatus, MutationCommitted,
    MutationPrepared, MutationSubject, ReceiptStatus, Session, SessionLogEntry,
    SessionStreamRecord, TaskParentVerificationRecorded, TaskParticipantAttemptEntry,
    TaskParticipantAttemptId, TaskParticipantAttemptStatus, TaskParticipantPurpose, TaskPlanEntry,
    TaskPlanStatus, TaskStepId, TaskStepMode, TypedDomainEvent, TypedStoredEventDecode,
    VerificationPolicyChangedEntry, VerificationReceipt, VerificationRecordedEntry,
    WorkspaceMutationDetected, decode_typed_stored_event,
};

/// Projection schema for R51.2 execution and evidence lineage.
pub const INTENT_LINEAGE_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// Why the latest execution cannot advance beyond read-only provenance.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IntentLineageReadOnlyReasonV1 {
    MissingChangeSet,
    MissingParentMutation,
    GitRefAdvance,
    StaleParentSnapshot,
}

/// Public-safe R51.2 summary for one accepted intent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentLineageSummaryV1 {
    pub application_state: Option<IntentApplicationState>,
    pub advisory_criterion_count: u32,
    pub system_verified_criterion_count: u32,
    pub read_only_reason: Option<IntentLineageReadOnlyReasonV1>,
}

/// Replayed state for one concrete Task or Chat execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentExecutionLineageV1 {
    pub stack_version: crate::IntentStackVersion,
    pub binding: IntentExecutionBindingV1,
    pub binding_event_id: String,
    pub binding_stream_sequence: u64,
    pub changeset_ids: Vec<ChangeSetId>,
    pub parent_mutation_event_id: Option<String>,
    pub parent_snapshot_id: Option<String>,
    pub read_only_reason: Option<IntentLineageReadOnlyReasonV1>,
}

/// Append-only R51.2 lineage projection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentLineageProjectionV1 {
    pub executions: BTreeMap<IntentExecutionId, IntentExecutionLineageV1>,
    pub execution_order: Vec<IntentExecutionId>,
    evidence: Vec<IntentCriterionEvidenceV1>,
    current_parent_snapshot_id: Option<String>,
}

impl IntentLineageProjectionV1 {
    /// Replays execution, ChangeSet, parent mutation and verification evidence.
    ///
    /// Invalid event identity fails closed. Missing or stale external lineage remains visible as
    /// read-only provenance rather than turning into executable authority.
    pub fn from_records(
        records: &[SessionStreamRecord],
        admission: &IntentStackProjectionV1,
    ) -> Result<Self> {
        let facts = DurableLineageFacts::from_records(records)?;
        let Some(accepted) = admission.latest_accepted_plan() else {
            if records.iter().any(|record| {
                matches!(
                    DurableEventType::from_event_type(&record.stored_event().event_type),
                    Some(
                        DurableEventType::IntentExecutionBound
                            | DurableEventType::IntentChangeSetBound
                            | DurableEventType::IntentVerificationLinked
                    )
                )
            }) {
                bail!("Intent lineage events exist without an accepted IntentPlan");
            }
            return Ok(Self::default());
        };
        let mut projection = Self {
            current_parent_snapshot_id: facts.current_parent_snapshot(&accepted.plan.workspace_id),
            ..Self::default()
        };
        for record in records {
            let event = record.stored_event();
            let Some(kind) = DurableEventType::from_event_type(&event.event_type) else {
                continue;
            };
            if !matches!(
                kind,
                DurableEventType::IntentExecutionBound
                    | DurableEventType::IntentChangeSetBound
                    | DurableEventType::IntentVerificationLinked
            ) {
                continue;
            }
            let intent_event = match decode_typed_stored_event(event.clone())? {
                TypedStoredEventDecode::Known(event) => match *event {
                    TypedDomainEvent::Intent(intent_event) => intent_event,
                    _ => bail!("R51.2 wire type did not decode as an Intent event"),
                },
                TypedStoredEventDecode::UnknownNonCritical(_) => {
                    bail!("R51.2 recovery-critical event decoded as unknown")
                }
            };
            match intent_event {
                IntentEventV1::ExecutionBound {
                    stack_id,
                    stack_version,
                    binding,
                    ..
                } => {
                    let execution_plan = admission.accepted_plan(stack_version).context(
                        "Intent execution binding references an unaccepted plan version",
                    )?;
                    if stack_id != execution_plan.plan.stack_id
                        || execution_plan.accepted_stream_sequence >= event.stream_sequence
                        || !execution_plan
                            .plan
                            .intents
                            .iter()
                            .any(|intent| intent.intent_ref == binding.intent_ref)
                    {
                        bail!("Intent execution binding references an unaccepted stack or intent");
                    }
                    facts.validate_execution_source(&binding, execution_plan)?;
                    if projection.executions.contains_key(&binding.execution_id) {
                        bail!("Intent execution id was bound more than once");
                    }
                    projection
                        .execution_order
                        .push(binding.execution_id.clone());
                    projection.executions.insert(
                        binding.execution_id.clone(),
                        IntentExecutionLineageV1 {
                            stack_version,
                            binding,
                            binding_event_id: event.event_id.clone(),
                            binding_stream_sequence: event.stream_sequence,
                            changeset_ids: Vec::new(),
                            parent_mutation_event_id: None,
                            parent_snapshot_id: None,
                            read_only_reason: None,
                        },
                    );
                }
                IntentEventV1::ChangeSetBound {
                    intent_ref,
                    execution_id,
                    changeset_ids,
                    ..
                } => {
                    let execution = projection
                        .executions
                        .get_mut(&execution_id)
                        .context("Intent ChangeSet binding precedes its execution binding")?;
                    if execution.binding.intent_ref != intent_ref {
                        bail!("Intent ChangeSet binding references another intent");
                    }
                    if !execution.changeset_ids.is_empty() {
                        bail!("Intent execution was bound to ChangeSets more than once");
                    }
                    let ids = changeset_ids
                        .into_iter()
                        .map(ChangeSetId::new)
                        .collect::<Result<Vec<_>>>()?;
                    facts.validate_changesets(&ids)?;
                    execution.changeset_ids = ids;
                }
                IntentEventV1::VerificationLinked { evidence, .. } => {
                    for item in &evidence {
                        let evidence_plan = admission
                            .accepted_plan_for_intent_at(&item.intent_ref, event.stream_sequence)
                            .context(
                                "Intent verification evidence references an unaccepted intent",
                            )?;
                        if !evidence_plan.plan.intents.iter().any(|intent| {
                            intent.intent_ref == item.intent_ref
                                && intent
                                    .acceptance_criteria
                                    .iter()
                                    .any(|criterion| criterion.criterion_id == item.criterion_id)
                        }) {
                            bail!(
                                "Intent verification evidence references an unaccepted criterion"
                            );
                        }
                        facts.validate_basic_evidence(item)?;
                        if !projection.evidence.iter().any(|existing| existing == item) {
                            projection.evidence.push(item.clone());
                        }
                    }
                }
                _ => bail!("R51.2 event type carried another Intent payload"),
            }
        }
        projection.resolve_parent_lineage(&facts, admission)?;
        Ok(projection)
    }

    #[must_use]
    pub fn execution(&self, execution_id: &IntentExecutionId) -> Option<&IntentExecutionLineageV1> {
        self.executions.get(execution_id)
    }

    #[must_use]
    pub fn latest_execution_for(
        &self,
        intent_ref: &IntentVersionRef,
    ) -> Option<&IntentExecutionLineageV1> {
        self.execution_order.iter().rev().find_map(|execution_id| {
            self.executions
                .get(execution_id)
                .filter(|execution| &execution.binding.intent_ref == intent_ref)
        })
    }

    /// Returns current system-verification receipt identities for exact operation invalidation.
    ///
    /// Advisory or stale evidence is intentionally excluded.
    #[must_use]
    pub fn current_system_verification_receipt_ids(
        &self,
        intent_ref: &IntentVersionRef,
    ) -> Vec<String> {
        let Some(execution) = self.latest_execution_for(intent_ref) else {
            return Vec::new();
        };
        let changeset_ids = execution
            .changeset_ids
            .iter()
            .map(ChangeSetId::as_str)
            .collect::<BTreeSet<_>>();
        self.evidence
            .iter()
            .filter(|item| {
                if &item.intent_ref != intent_ref
                    || item.level != IntentCriterionEvidenceLevel::SystemVerified
                    || execution.parent_snapshot_id.as_deref()
                        != Some(item.parent_snapshot_id.as_str())
                    || self.current_parent_snapshot_id.as_deref()
                        != Some(item.parent_snapshot_id.as_str())
                {
                    return false;
                }
                item.changeset_ids
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    == changeset_ids
            })
            .map(|item| item.receipt_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub fn summary_for(&self, intent_ref: &IntentVersionRef) -> IntentLineageSummaryV1 {
        let Some(execution) = self.latest_execution_for(intent_ref) else {
            return IntentLineageSummaryV1 {
                application_state: Some(IntentApplicationState::Unapplied),
                ..IntentLineageSummaryV1::default()
            };
        };
        let application_state = if execution.changeset_ids.is_empty()
            || execution.parent_mutation_event_id.is_none()
            || execution.read_only_reason.is_some()
        {
            IntentApplicationState::ReadOnly
        } else {
            // R51.2 proves parent lineage only. R51.3 must materialize a layer before `Applied`.
            IntentApplicationState::NeedsReview
        };
        let mut summary = IntentLineageSummaryV1 {
            application_state: Some(application_state),
            read_only_reason: execution.read_only_reason.clone(),
            ..IntentLineageSummaryV1::default()
        };
        let changeset_ids = execution
            .changeset_ids
            .iter()
            .map(ChangeSetId::as_str)
            .collect::<BTreeSet<_>>();
        let mut advisory_criteria = BTreeSet::new();
        let mut system_criteria = BTreeSet::new();
        for item in self
            .evidence
            .iter()
            .filter(|item| &item.intent_ref == intent_ref)
        {
            match item.level {
                IntentCriterionEvidenceLevel::Advisory => {
                    advisory_criteria.insert(item.criterion_id.clone());
                }
                IntentCriterionEvidenceLevel::SystemVerified => {
                    let evidence_changesets = item
                        .changeset_ids
                        .iter()
                        .map(String::as_str)
                        .collect::<BTreeSet<_>>();
                    let current = execution.parent_snapshot_id.as_deref()
                        == Some(item.parent_snapshot_id.as_str())
                        && self.current_parent_snapshot_id.as_deref()
                            == Some(item.parent_snapshot_id.as_str())
                        && evidence_changesets == changeset_ids;
                    if current {
                        system_criteria.insert(item.criterion_id.clone());
                    } else {
                        summary.application_state = Some(IntentApplicationState::ReadOnly);
                        summary.read_only_reason =
                            Some(IntentLineageReadOnlyReasonV1::StaleParentSnapshot);
                    }
                }
            }
        }
        summary.advisory_criterion_count =
            u32::try_from(advisory_criteria.len()).unwrap_or(u32::MAX);
        summary.system_verified_criterion_count =
            u32::try_from(system_criteria.len()).unwrap_or(u32::MAX);
        summary
    }

    fn resolve_parent_lineage(
        &mut self,
        facts: &DurableLineageFacts,
        admission: &IntentStackProjectionV1,
    ) -> Result<()> {
        for execution in self.executions.values_mut() {
            let accepted = admission
                .accepted_plan(execution.stack_version)
                .context("Intent execution lineage lost its accepted plan version")?;
            if execution.changeset_ids.is_empty() {
                execution.read_only_reason = Some(IntentLineageReadOnlyReasonV1::MissingChangeSet);
                continue;
            }
            let resolved = match &execution.binding.origin {
                IntentExecutionOriginV1::Task {
                    task_id,
                    task_plan_version,
                    step_id,
                    ..
                } => facts.task_parent_lineage(
                    task_id,
                    *task_plan_version,
                    step_id,
                    &execution.changeset_ids,
                    &accepted.plan.workspace_id,
                ),
                IntentExecutionOriginV1::Chat { .. } => facts.chat_parent_lineage(
                    execution,
                    &execution.changeset_ids,
                    &accepted.plan.workspace_id,
                ),
            };
            match resolved {
                ParentLineage::Applied {
                    mutation_event_id,
                    snapshot_id,
                } => {
                    execution.parent_mutation_event_id = Some(mutation_event_id);
                    execution.parent_snapshot_id = Some(snapshot_id);
                }
                ParentLineage::ReadOnly(reason) => execution.read_only_reason = Some(reason),
            }
        }
        for item in &self.evidence {
            if item.level == IntentCriterionEvidenceLevel::SystemVerified {
                let execution = self
                    .latest_execution_for(&item.intent_ref)
                    .context("system-verified criterion has no execution lineage")?;
                facts.validate_system_evidence(item, execution)?;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentLineage {
    Applied {
        mutation_event_id: String,
        snapshot_id: String,
    },
    ReadOnly(IntentLineageReadOnlyReasonV1),
}

#[derive(Debug, Clone)]
struct EventFact<T> {
    event_id: String,
    stream_sequence: u64,
    value: T,
}

#[derive(Debug, Clone, Default)]
struct DurableLineageFacts {
    task_plans: Vec<EventFact<TaskPlanEntry>>,
    task_attempts: Vec<EventFact<TaskParticipantAttemptEntry>>,
    task_projection: crate::TaskStateProjection,
    agent_attempts: Vec<EventFact<crate::AgentRunAttemptStartedEntry>>,
    agent_interruptions: Vec<EventFact<crate::AgentRunInterruptedEntry>>,
    changesets: BTreeMap<ChangeSetId, EventFact<ChangeSet>>,
    changeset_results: BTreeMap<ChangeSetId, EventFact<ChangeSetResult>>,
    mutation_prepared: Vec<EventFact<MutationPrepared>>,
    mutation_committed: Vec<EventFact<MutationCommitted>>,
    mutation_batches_started: Vec<EventFact<MutationBatchStarted>>,
    mutation_batches_finished: Vec<EventFact<MutationBatchFinished>>,
    workspace_mutations: Vec<EventFact<WorkspaceMutationDetected>>,
    integration_plans: Vec<EventFact<IntegrationPlanRecorded>>,
    promotions: Vec<EventFact<IntegrationPromotionRecorded>>,
    integration_projection: crate::IntegrationProjection,
    policies: Vec<EventFact<VerificationPolicyChangedEntry>>,
    verification_receipts: Vec<EventFact<VerificationRecordedEntry>>,
    parent_verifications: Vec<EventFact<TaskParentVerificationRecorded>>,
}

impl DurableLineageFacts {
    fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let mut facts = Self::default();
        for record in records {
            let event = record.stored_event();
            let event_id = event.event_id.clone();
            let stream_sequence = event.stream_sequence;
            match DurableEventType::from_event_type(&event.event_type) {
                Some(DurableEventType::MutationPrepared) => {
                    facts.mutation_prepared.push(EventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode mutation prepare lineage")?,
                    });
                    continue;
                }
                Some(DurableEventType::MutationCommitted) => {
                    facts.mutation_committed.push(EventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode mutation commit lineage")?,
                    });
                    continue;
                }
                Some(DurableEventType::WorkspaceMutationDetected) => {
                    facts.workspace_mutations.push(EventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode workspace mutation lineage")?,
                    });
                    continue;
                }
                Some(DurableEventType::MutationBatchStarted) => {
                    facts.mutation_batches_started.push(EventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode mutation batch start lineage")?,
                    });
                    continue;
                }
                Some(DurableEventType::MutationBatchFinished) => {
                    facts.mutation_batches_finished.push(EventFact {
                        event_id,
                        stream_sequence,
                        value: serde_json::from_value(event.payload.clone())
                            .context("failed to decode mutation batch terminal lineage")?,
                    });
                    continue;
                }
                _ => {}
            }
            let Some(SessionLogEntry::Control(control)) = record.session_log_entry()? else {
                continue;
            };
            facts.integration_projection.apply_control_entry(&control);
            facts.task_projection.apply_control_entry(&control);
            macro_rules! push_fact {
                ($target:expr, $value:expr) => {
                    $target.push(EventFact {
                        event_id,
                        stream_sequence,
                        value: $value,
                    })
                };
            }
            match control {
                ControlEntry::TaskPlan(value) => push_fact!(facts.task_plans, value),
                // RFC-0067 adoption and RFC-0069 materialization both carry the accepted
                // TaskPlan authority used to validate subsequent task intent executions.
                ControlEntry::PlanExecutionAdoptedV1(adoption) => {
                    push_fact!(
                        facts.task_plans,
                        adoption.adopted_candidate.task_plan.clone()
                    )
                }
                ControlEntry::TaskMaterializationPreparedV1(materialization) => {
                    push_fact!(
                        facts.task_plans,
                        materialization.adopted_candidate.task_plan.clone()
                    )
                }
                ControlEntry::TaskParticipantAttempt(value) => {
                    push_fact!(facts.task_attempts, value)
                }
                ControlEntry::AgentRunAttemptStarted(value) => {
                    push_fact!(facts.agent_attempts, value)
                }
                ControlEntry::AgentRunInterrupted(value) => {
                    push_fact!(facts.agent_interruptions, value)
                }
                ControlEntry::ChangeSetProposed(value) => {
                    let id = value.id.clone();
                    if facts
                        .changesets
                        .insert(
                            id,
                            EventFact {
                                event_id,
                                stream_sequence,
                                value,
                            },
                        )
                        .is_some()
                    {
                        bail!("durable stream repeats a ChangeSet proposal");
                    }
                }
                ControlEntry::ChangeSetApplied(value) => {
                    let id = value.id.clone();
                    if facts
                        .changeset_results
                        .insert(
                            id,
                            EventFact {
                                event_id,
                                stream_sequence,
                                value,
                            },
                        )
                        .is_some()
                    {
                        bail!("durable stream repeats a ChangeSet result");
                    }
                }
                ControlEntry::IntegrationPlanRecorded(value) => {
                    push_fact!(facts.integration_plans, value)
                }
                ControlEntry::IntegrationPromotionRecorded(value) => {
                    push_fact!(facts.promotions, value)
                }
                ControlEntry::VerificationPolicyChanged(value) => {
                    if value.policy.stable_hash()? != value.policy_hash {
                        bail!("Intent lineage verification policy hash is inconsistent");
                    }
                    for check in &value.policy.required_checks {
                        check.validate_shape()?;
                    }
                    push_fact!(facts.policies, value)
                }
                ControlEntry::VerificationRecorded(value) => {
                    value.receipt.receipt.validate_source_identity()?;
                    push_fact!(facts.verification_receipts, value)
                }
                ControlEntry::TaskParentVerificationRecorded(value) => {
                    value.validate()?;
                    push_fact!(facts.parent_verifications, value)
                }
                _ => {}
            }
        }
        Ok(facts)
    }

    fn validate_execution_source(
        &self,
        binding: &IntentExecutionBindingV1,
        accepted: &crate::AcceptedIntentPlanProjectionV1,
    ) -> Result<()> {
        match &binding.origin {
            IntentExecutionOriginV1::Task {
                task_id,
                task_plan_version,
                step_id,
                attempt_id,
            } => {
                let attempt_id = attempt_id
                    .as_deref()
                    .context("Task Intent execution requires an exact attempt id")?;
                let source = self
                    .task_attempts
                    .iter()
                    .find(|fact| fact.event_id == binding.source_event_id)
                    .context("Task Intent execution source event is not a participant attempt")?;
                let attempt = &source.value;
                if attempt.status != TaskParticipantAttemptStatus::Started
                    || attempt.purpose != TaskParticipantPurpose::Step
                    || attempt.attempt_id.as_str() != attempt_id
                    || attempt.task_id.as_str() != task_id
                    || attempt.plan_version != Some(*task_plan_version)
                    || attempt.step_id.as_ref().map(TaskStepId::as_str) != Some(step_id.as_str())
                {
                    bail!("Task Intent execution does not match its exact started attempt");
                }
                let step = self
                    .task_plans
                    .iter()
                    .rev()
                    .find(|fact| {
                        fact.value.status == TaskPlanStatus::Accepted
                            && fact.value.task_id.as_str() == task_id
                            && fact.value.plan_version == *task_plan_version
                    })
                    .and_then(|fact| {
                        fact.value
                            .steps
                            .iter()
                            .find(|step| step.step_id.as_str() == step_id)
                    })
                    .context("Task Intent execution has no accepted TaskPlan step")?;
                let task_state = self
                    .task_projection
                    .tasks
                    .get(&attempt.task_id)
                    .context("Task Intent execution has no task projection")?;
                if task_state.participant_conflicts != 0
                    || task_state.plans.get(task_plan_version).is_none_or(|plan| {
                        plan.status != TaskPlanStatus::Accepted
                            || plan.graph_validation_error.is_some()
                    })
                {
                    bail!("Task Intent execution belongs to an inconsistent Task projection");
                }
                if !step.intent_refs.contains(&binding.intent_ref) {
                    bail!("Task Intent execution is not declared by its TaskPlan step");
                }
                if step.effective_mode() == TaskStepMode::Write
                    && (step.intent_refs.len() != 1
                        || binding.binding_kind != IntentExecutionBindingKind::Mutation)
                {
                    bail!("Task write execution must bind exactly one mutation intent");
                }
            }
            IntentExecutionOriginV1::Chat {
                root_logical_run_id,
                source_turn_id,
                attempt_id,
            } => {
                let attempt_id = attempt_id
                    .as_deref()
                    .context("Chat Intent execution requires an exact attempt id")?;
                let source = self
                    .agent_attempts
                    .iter()
                    .find(|fact| fact.event_id == binding.source_event_id)
                    .context("Chat Intent execution source event is not an agent-run attempt")?;
                if source.value.thread_id.as_str() != root_logical_run_id
                    || source.value.attempt_id.as_str() != attempt_id
                    || source.value.background
                    || source_turn_id != &accepted.source_turn_id
                    || accepted.task_plan_binding.is_some()
                    || accepted.plan.intents.len() != 1
                    || binding.binding_kind != IntentExecutionBindingKind::Mutation
                {
                    bail!("Chat Intent execution does not match its accepted root attempt");
                }
            }
        }
        Ok(())
    }

    fn validate_changesets(&self, ids: &[ChangeSetId]) -> Result<()> {
        for id in ids {
            if !self.changesets.contains_key(id) {
                bail!("Intent ChangeSet binding references an unknown proposal");
            }
        }
        Ok(())
    }

    fn validate_basic_evidence(&self, evidence: &IntentCriterionEvidenceV1) -> Result<()> {
        let receipt = self
            .receipt_from_source_event(&evidence.source_event_id, &evidence.receipt_id)
            .context("Intent verification source event has no matching receipt")?;
        if receipt.receipt.receipt_id != evidence.receipt_id
            || receipt.binding.workspace_snapshot_id != evidence.parent_snapshot_id
        {
            bail!("Intent verification evidence does not match its receipt snapshot");
        }
        Ok(())
    }

    fn validate_system_evidence(
        &self,
        evidence: &IntentCriterionEvidenceV1,
        execution: &IntentExecutionLineageV1,
    ) -> Result<()> {
        let receipt = self
            .receipt_from_source_event(&evidence.source_event_id, &evidence.receipt_id)
            .context("system Intent evidence source receipt is unavailable")?;
        if receipt.check_status != ReceiptStatus::Succeeded
            || receipt.receipt.status != ReceiptStatus::Succeeded
            || receipt.mutates_verification_scope
            || receipt.binding.execution_backend.is_none()
            || receipt.binding.workspace_snapshot_id != evidence.parent_snapshot_id
            || receipt.receipt.policy_hash.as_deref()
                != Some(evidence.verification_policy_digest.as_str())
        {
            bail!("system Intent evidence receipt is not successful and parent-bound");
        }
        if execution.parent_snapshot_id.as_deref() != Some(evidence.parent_snapshot_id.as_str()) {
            bail!("system Intent evidence does not bind the execution parent snapshot");
        }
        if let Some(parent) = self
            .parent_verifications
            .iter()
            .find(|fact| fact.event_id == evidence.source_event_id)
            && (parent.value.promoted_snapshot_id != evidence.parent_snapshot_id
                || parent.value.policy_digest != evidence.verification_policy_digest.as_str()
                || parent.value.verdict != crate::VerificationVerdict::Passed
                || self
                    .integration_projection
                    .plans
                    .get(&parent.value.plan_id)
                    .is_none_or(|state| state.inconsistent))
        {
            bail!("task parent verification is not valid for the Intent evidence");
        }
        let policy = self
            .policies
            .iter()
            .rev()
            .find(|fact| fact.value.policy_hash == evidence.verification_policy_digest.as_str())
            .context("system Intent evidence has no durable verification policy")?;
        let check = policy
            .value
            .policy
            .required_checks
            .iter()
            .find(|check| {
                check.check_spec_id == receipt.check_spec_id
                    && check.check_spec_hash == receipt.binding.check_spec_hash
            })
            .context("system Intent evidence receipt is outside its verification policy")?;
        if !check.covers_intent_criterion(&evidence.intent_ref, &evidence.criterion_id) {
            bail!("verification check does not explicitly cover the Intent criterion");
        }
        let execution_changesets = execution
            .changeset_ids
            .iter()
            .map(ChangeSetId::as_str)
            .collect::<BTreeSet<_>>();
        let evidence_changesets = evidence
            .changeset_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        if execution_changesets != evidence_changesets {
            bail!("system Intent evidence does not bind the execution ChangeSets");
        }
        Ok(())
    }

    fn receipt_from_source_event(
        &self,
        source_event_id: &str,
        receipt_id: &str,
    ) -> Option<&VerificationReceipt> {
        if let Some(recorded) = self
            .verification_receipts
            .iter()
            .find(|fact| fact.event_id == source_event_id)
        {
            return (recorded.value.receipt.receipt.receipt_id == receipt_id)
                .then_some(&recorded.value.receipt);
        }
        self.parent_verifications
            .iter()
            .find(|fact| fact.event_id == source_event_id)
            .and_then(|fact| {
                fact.value
                    .receipts
                    .iter()
                    .find(|receipt| receipt.receipt.receipt_id == receipt_id)
            })
    }

    fn task_parent_lineage(
        &self,
        task_id: &str,
        task_plan_version: u32,
        step_id: &str,
        changeset_ids: &[ChangeSetId],
        workspace_id: &str,
    ) -> ParentLineage {
        let changesets = changeset_ids.iter().collect::<BTreeSet<_>>();
        let Some(plan) = self.integration_plans.iter().rev().find(|fact| {
            fact.value.plan.task_id.as_str() == task_id
                && fact.value.plan.plan_version == task_plan_version
                && changesets.iter().all(|id| {
                    fact.value.plan.proposals.iter().any(|proposal| {
                        &proposal.change_set_id == *id && proposal.step_id.as_str() == step_id
                    })
                })
        }) else {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        };
        if self
            .integration_projection
            .plans
            .get(&plan.value.plan.plan_id)
            .is_none_or(|state| state.inconsistent)
        {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        }
        let Some(promotion) = self.promotions.iter().rev().find(|fact| {
            fact.value.plan_id == plan.value.plan.plan_id
                && fact.value.status == IntegrationPromotionStatus::Promoted
        }) else {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        };
        let snapshot_id = match (&promotion.value.target, &promotion.value.effect) {
            (
                IntegrationPromotionTarget::WorkspaceApply { .. },
                Some(IntegrationPromotionEffect::WorkspaceApplied {
                    promoted_snapshot_id,
                    ..
                }),
            ) => promoted_snapshot_id,
            (IntegrationPromotionTarget::GitRefAdvance { .. }, _) => {
                return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::GitRefAdvance);
            }
            _ => {
                return ParentLineage::ReadOnly(
                    IntentLineageReadOnlyReasonV1::MissingParentMutation,
                );
            }
        };
        let Some(batch) = self.mutation_batches_finished.iter().rev().find(|fact| {
            fact.stream_sequence < promotion.stream_sequence
                && fact.value.status == MutationBatchStatus::Applied
                && fact.value.failed_operations.is_empty()
                && self.batch_applies_changesets(fact, changeset_ids, workspace_id)
        }) else {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        };
        ParentLineage::Applied {
            mutation_event_id: batch.event_id.clone(),
            snapshot_id: snapshot_id.clone(),
        }
    }

    fn batch_applies_changesets(
        &self,
        terminal: &EventFact<MutationBatchFinished>,
        changeset_ids: &[ChangeSetId],
        workspace_id: &str,
    ) -> bool {
        let Some(started) = self
            .mutation_batches_started
            .iter()
            .rev()
            .find(|fact| fact.value.batch_id == terminal.value.batch_id)
        else {
            return false;
        };
        if started.stream_sequence >= terminal.stream_sequence {
            return false;
        }
        let committed_operations = terminal
            .value
            .committed_operations
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        changeset_ids.iter().all(|id| {
            let Some(change_set) = self.changesets.get(id) else {
                return false;
            };
            self.changeset_results
                .get(id)
                .is_some_and(|result| result.value.status == ChangeSetResultStatus::Applied)
                && change_set.value.files.iter().all(|file| {
                    if file.action == ChangeSetFileAction::Rename {
                        return false;
                    }
                    self.mutation_prepared.iter().any(|prepared| {
                        prepared.value.batch_id.as_deref() == Some(terminal.value.batch_id.as_str())
                            && prepared.stream_sequence > started.stream_sequence
                            && prepared.stream_sequence < terminal.stream_sequence
                            && prepared.value.workspace_id == workspace_id
                            && prepared.value.before_hash == file.before_hash
                            && prepared.value.intended_after_hash == file.after_hash
                            && mutation_subject_matches_path(&prepared.value.subject, &file.path)
                            && started
                                .value
                                .expected_subjects
                                .contains(&prepared.value.subject)
                            && committed_operations.contains(prepared.value.operation_id.as_str())
                            && self.mutation_committed.iter().any(|committed| {
                                committed.value.operation_id == prepared.value.operation_id
                                    && committed.value.batch_id == prepared.value.batch_id
                                    && committed.stream_sequence > prepared.stream_sequence
                                    && committed.stream_sequence < terminal.stream_sequence
                                    && committed.value.workspace_id.as_deref() == Some(workspace_id)
                                    && committed.value.observed_after_hash == file.after_hash
                                    && mutation_subject_matches_path(
                                        &committed.value.committed_subject,
                                        &file.path,
                                    )
                            })
                    })
                })
        })
    }

    fn chat_parent_lineage(
        &self,
        execution: &IntentExecutionLineageV1,
        changeset_ids: &[ChangeSetId],
        workspace_id: &str,
    ) -> ParentLineage {
        if changeset_ids.len() != 1 {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        }
        let id = &changeset_ids[0];
        let Some(change_set) = self.changesets.get(id) else {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingChangeSet);
        };
        if self
            .changeset_results
            .get(id)
            .is_none_or(|result| result.value.status != ChangeSetResultStatus::Applied)
            || change_set.value.files.len() != 1
        {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        }
        let file = &change_set.value.files[0];
        let attempt_window_end = self.chat_attempt_window_end(&execution.binding);
        let Some((prepared, committed)) = self.mutation_prepared.iter().find_map(|prepared| {
            if prepared.stream_sequence <= execution.binding_stream_sequence
                || attempt_window_end
                    .is_some_and(|window_end| prepared.stream_sequence >= window_end)
                || prepared.value.workspace_id != workspace_id
                || !mutation_subject_matches_path(&prepared.value.subject, &file.path)
                || prepared.value.before_hash != file.before_hash
                || prepared.value.intended_after_hash != file.after_hash
            {
                return None;
            }
            self.mutation_committed
                .iter()
                .find(|committed| {
                    committed.value.operation_id == prepared.value.operation_id
                        && attempt_window_end
                            .is_none_or(|window_end| committed.stream_sequence < window_end)
                        && committed.value.workspace_id.as_deref() == Some(workspace_id)
                        && committed.value.observed_after_hash == file.after_hash
                        && mutation_subject_matches_path(
                            &committed.value.committed_subject,
                            &file.path,
                        )
                })
                .map(|committed| (prepared, committed))
        }) else {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        };
        if committed.stream_sequence <= prepared.stream_sequence {
            return ParentLineage::ReadOnly(IntentLineageReadOnlyReasonV1::MissingParentMutation);
        }
        ParentLineage::Applied {
            mutation_event_id: committed.event_id.clone(),
            snapshot_id: committed.value.workspace_snapshot_id.clone(),
        }
    }

    fn chat_attempt_window_end(&self, binding: &IntentExecutionBindingV1) -> Option<u64> {
        let IntentExecutionOriginV1::Chat {
            root_logical_run_id,
            attempt_id,
            ..
        } = &binding.origin
        else {
            return None;
        };
        let attempt_id = attempt_id.as_deref()?;
        let source_sequence = self
            .agent_attempts
            .iter()
            .find(|fact| fact.event_id == binding.source_event_id)?
            .stream_sequence;
        self.agent_attempts
            .iter()
            .filter(|fact| {
                fact.stream_sequence > source_sequence
                    && fact.value.thread_id.as_str() == root_logical_run_id
            })
            .map(|fact| fact.stream_sequence)
            .chain(
                self.agent_interruptions
                    .iter()
                    .filter(|fact| {
                        fact.stream_sequence > source_sequence
                            && fact.value.thread_id.as_str() == root_logical_run_id
                            && fact.value.attempt_id.as_str() == attempt_id
                    })
                    .map(|fact| fact.stream_sequence),
            )
            .min()
    }

    fn current_parent_snapshot(&self, workspace_id: &str) -> Option<String> {
        let committed = self
            .mutation_committed
            .iter()
            .filter(|fact| fact.value.workspace_id.as_deref() == Some(workspace_id))
            .map(|fact| {
                (
                    fact.stream_sequence,
                    Some(fact.value.workspace_snapshot_id.clone()),
                )
            });
        let detected = self
            .workspace_mutations
            .iter()
            .filter(|fact| fact.value.workspace_id == workspace_id)
            .map(|fact| {
                (
                    fact.stream_sequence,
                    fact.value.to_workspace_snapshot_id.clone(),
                )
            });
        committed
            .chain(detected)
            .max_by_key(|(sequence, _)| *sequence)
            .and_then(|(_, snapshot)| snapshot)
    }
}

fn mutation_subject_matches_path(subject: &MutationSubject, expected_path: &str) -> bool {
    matches!(
        subject,
        MutationSubject::File { path, .. }
            if crate::mutation::portable_relative_path(path).as_deref() == Some(expected_path)
    )
}

/// Result of an idempotent R51.2 durable append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentLineageWriteOutcomeV1 {
    pub appended: bool,
    pub execution_id: Option<IntentExecutionId>,
}

/// Binds one accepted intent to an exact started Task step attempt.
///
/// # Errors
///
/// Returns an error when the TaskPlan step does not carry the ref, the source attempt is not exact,
/// or the durable stream already binds the deterministic execution id differently.
pub fn append_task_intent_execution_binding(
    session: &Session,
    intent_ref: IntentVersionRef,
    task_id: &crate::TaskId,
    task_plan_version: u32,
    step_id: &TaskStepId,
    attempt_id: &TaskParticipantAttemptId,
) -> Result<IntentLineageWriteOutcomeV1> {
    let records = durable_records(session)?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let accepted = admission
        .latest_accepted_plan()
        .context("Task Intent execution requires an accepted IntentPlan")?;
    let facts = DurableLineageFacts::from_records(&records)?;
    let source = facts
        .task_attempts
        .iter()
        .find(|fact| {
            fact.value.status == TaskParticipantAttemptStatus::Started
                && fact.value.attempt_id == *attempt_id
                && fact.value.task_id == *task_id
                && fact.value.plan_version == Some(task_plan_version)
                && fact.value.step_id.as_ref() == Some(step_id)
        })
        .context("Task Intent execution has no exact started participant attempt")?;
    let origin = IntentExecutionOriginV1::Task {
        task_id: task_id.as_str().to_owned(),
        task_plan_version,
        step_id: step_id.as_str().to_owned(),
        attempt_id: Some(attempt_id.as_str().to_owned()),
    };
    let binding = IntentExecutionBindingV1 {
        execution_id: deterministic_execution_id(accepted, &intent_ref, &origin, &source.event_id)?,
        intent_ref,
        origin,
        binding_kind: IntentExecutionBindingKind::Mutation,
        source_event_id: source.event_id.clone(),
    };
    append_execution_binding(session, binding)
}

/// Binds one accepted Chat root to the exact foreground root-run attempt.
///
/// # Errors
///
/// Returns an error when the session does not have a single Chat root, source turn/run/attempt
/// mismatches, or the deterministic binding conflicts with durable state.
pub fn append_chat_intent_execution_binding(
    session: &Session,
    intent_ref: IntentVersionRef,
    root_logical_run_id: &str,
    source_turn_id: &str,
    attempt_id: &str,
) -> Result<IntentLineageWriteOutcomeV1> {
    let records = durable_records(session)?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let accepted = admission
        .latest_accepted_plan()
        .context("Chat Intent execution requires an accepted IntentPlan")?;
    let facts = DurableLineageFacts::from_records(&records)?;
    let source = facts
        .agent_attempts
        .iter()
        .find(|fact| {
            fact.value.thread_id.as_str() == root_logical_run_id
                && fact.value.attempt_id.as_str() == attempt_id
                && !fact.value.background
        })
        .context("Chat Intent execution has no exact foreground agent-run attempt")?;
    let origin = IntentExecutionOriginV1::Chat {
        root_logical_run_id: root_logical_run_id.to_owned(),
        source_turn_id: source_turn_id.to_owned(),
        attempt_id: Some(attempt_id.to_owned()),
    };
    let binding = IntentExecutionBindingV1 {
        execution_id: deterministic_execution_id(accepted, &intent_ref, &origin, &source.event_id)?,
        intent_ref,
        origin,
        binding_kind: IntentExecutionBindingKind::Mutation,
        source_event_id: source.event_id.clone(),
    };
    append_execution_binding(session, binding)
}

fn append_execution_binding(
    session: &Session,
    binding: IntentExecutionBindingV1,
) -> Result<IntentLineageWriteOutcomeV1> {
    let store = session
        .durable_store()
        .context("Intent execution binding requires a durable session")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let accepted = admission
        .latest_accepted_plan()
        .context("Intent execution binding requires an accepted IntentPlan")?;
    ensure_accepted_intent(accepted, &binding.intent_ref)?;
    DurableLineageFacts::from_records(&records)?.validate_execution_source(&binding, accepted)?;
    let event = IntentEventV1::ExecutionBound {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        stack_id: accepted.plan.stack_id.clone(),
        stack_version: accepted.plan.stack_version,
        binding: binding.clone(),
    };
    let event_tuple = intent_event_tuple(DurableEventType::IntentExecutionBound, &event)?;
    let predicate_binding = binding.clone();
    let appended = store
        .append_events_and_session_entries_if(vec![event_tuple], &[], move |records| {
            let admission = IntentStackProjectionV1::from_records(records)?;
            let projection = IntentLineageProjectionV1::from_records(records, &admission)?;
            if let Some(existing) = projection.execution(&predicate_binding.execution_id) {
                if existing.binding != predicate_binding {
                    bail!("deterministic Intent execution id conflicts with durable binding");
                }
                return Ok(false);
            }
            let accepted = admission
                .latest_accepted_plan()
                .context("Intent execution binding requires an accepted IntentPlan")?;
            ensure_accepted_intent(accepted, &predicate_binding.intent_ref)?;
            DurableLineageFacts::from_records(records)?
                .validate_execution_source(&predicate_binding, accepted)?;
            Ok(true)
        })?
        .is_some();
    Ok(IntentLineageWriteOutcomeV1 {
        appended,
        execution_id: Some(binding.execution_id),
    })
}

fn ensure_accepted_intent(
    accepted: &crate::AcceptedIntentPlanProjectionV1,
    intent_ref: &IntentVersionRef,
) -> Result<()> {
    if accepted
        .plan
        .intents
        .iter()
        .any(|intent| &intent.intent_ref == intent_ref)
    {
        Ok(())
    } else {
        bail!("Intent execution binding references an unaccepted intent")
    }
}

/// Binds existing durable ChangeSet proposals to one exact execution.
///
/// # Errors
///
/// Returns an error when the execution is unknown, a ChangeSet proposal is absent, or a prior
/// binding conflicts.
pub fn append_intent_changeset_binding(
    session: &Session,
    execution_id: &IntentExecutionId,
    changeset_ids: Vec<ChangeSetId>,
) -> Result<IntentLineageWriteOutcomeV1> {
    if changeset_ids.is_empty() {
        bail!("Intent execution requires at least one ChangeSet");
    }
    let store = session
        .durable_store()
        .context("Intent ChangeSet binding requires a durable session")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let projection = IntentLineageProjectionV1::from_records(&records, &admission)?;
    let execution = projection
        .execution(execution_id)
        .context("Intent ChangeSet binding references an unknown execution")?;
    DurableLineageFacts::from_records(&records)?.validate_changesets(&changeset_ids)?;
    let event = IntentEventV1::ChangeSetBound {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        intent_ref: execution.binding.intent_ref.clone(),
        execution_id: execution_id.clone(),
        changeset_ids: changeset_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect(),
    };
    let event_tuple = intent_event_tuple(DurableEventType::IntentChangeSetBound, &event)?;
    let predicate_execution_id = execution_id.clone();
    let predicate_ids = changeset_ids.clone();
    let appended = store
        .append_events_and_session_entries_if(vec![event_tuple], &[], move |records| {
            let admission = IntentStackProjectionV1::from_records(records)?;
            let projection = IntentLineageProjectionV1::from_records(records, &admission)?;
            let existing = projection
                .execution(&predicate_execution_id)
                .context("Intent ChangeSet binding references an unknown execution")?;
            if !existing.changeset_ids.is_empty() {
                if existing.changeset_ids != predicate_ids {
                    bail!("Intent execution already has a different ChangeSet binding");
                }
                return Ok(false);
            }
            DurableLineageFacts::from_records(records)?.validate_changesets(&predicate_ids)?;
            Ok(true)
        })?
        .is_some();
    Ok(IntentLineageWriteOutcomeV1 {
        appended,
        execution_id: Some(execution_id.clone()),
    })
}

/// Materializes a bounded one-file ChangeSet from exact controlled Chat mutation evidence and
/// binds it in the same writer batch.
///
/// No patch bytes or model-authored lineage are accepted. The mutation prepare/commit pair is the
/// sole source for path, action and hashes.
///
/// # Errors
///
/// Returns an error for a non-Chat execution, absolute/unsupported subject, mismatched mutation
/// pair, stale workspace identity, or a conflicting synthetic ChangeSet.
pub fn append_chat_direct_mutation_changeset_binding(
    session: &mut Session,
    execution_id: &IntentExecutionId,
    prepared_event_id: &str,
    committed_event_id: &str,
) -> Result<IntentLineageWriteOutcomeV1> {
    let store = session
        .durable_store()
        .context("Chat mutation projection requires a durable session")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let projection = IntentLineageProjectionV1::from_records(&records, &admission)?;
    let execution = projection
        .execution(execution_id)
        .context("Chat mutation projection references an unknown execution")?;
    if !matches!(
        execution.binding.origin,
        IntentExecutionOriginV1::Chat { .. }
    ) {
        bail!("direct mutation projection is only valid for Chat execution");
    }
    let accepted = admission
        .latest_accepted_plan()
        .context("Chat mutation projection requires an accepted IntentPlan")?;
    let facts = DurableLineageFacts::from_records(&records)?;
    let prepared = facts
        .mutation_prepared
        .iter()
        .find(|fact| fact.event_id == prepared_event_id)
        .context("Chat mutation projection has no exact prepare event")?;
    let committed = facts
        .mutation_committed
        .iter()
        .find(|fact| fact.event_id == committed_event_id)
        .context("Chat mutation projection has no exact commit event")?;
    let (change_set, result) = direct_mutation_changeset(
        execution_id,
        prepared,
        committed,
        &accepted.plan.workspace_id,
        execution.binding_stream_sequence,
        facts.chat_attempt_window_end(&execution.binding),
    )?;
    let event = IntentEventV1::ChangeSetBound {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        intent_ref: execution.binding.intent_ref.clone(),
        execution_id: execution_id.clone(),
        changeset_ids: vec![change_set.id.as_str().to_owned()],
    };
    let event_tuple = intent_event_tuple(DurableEventType::IntentChangeSetBound, &event)?;
    let entries = [
        SessionLogEntry::Control(ControlEntry::ChangeSetProposed(change_set.clone())),
        SessionLogEntry::Control(ControlEntry::ChangeSetApplied(result.clone())),
    ];
    let predicate_execution_id = execution_id.clone();
    let predicate_change_set = change_set.clone();
    let appended = store
        .append_events_and_session_entries_if(vec![event_tuple], &entries, move |records| {
            let admission = IntentStackProjectionV1::from_records(records)?;
            let projection = IntentLineageProjectionV1::from_records(records, &admission)?;
            let execution = projection
                .execution(&predicate_execution_id)
                .context("Chat mutation projection references an unknown execution")?;
            if !execution.changeset_ids.is_empty() {
                if execution.changeset_ids != vec![predicate_change_set.id.clone()] {
                    bail!("Chat execution already has a different ChangeSet binding");
                }
                let facts = DurableLineageFacts::from_records(records)?;
                if facts
                    .changesets
                    .get(&predicate_change_set.id)
                    .is_none_or(|fact| fact.value != predicate_change_set)
                {
                    bail!("synthetic Chat ChangeSet conflicts with durable state");
                }
                return Ok(false);
            }
            if DurableLineageFacts::from_records(records)?
                .changesets
                .contains_key(&predicate_change_set.id)
            {
                bail!("synthetic Chat ChangeSet id already exists");
            }
            Ok(true)
        })?
        .is_some();
    if appended {
        session.record_durably_appended_control(ControlEntry::ChangeSetProposed(change_set));
        session.record_durably_appended_control(ControlEntry::ChangeSetApplied(result));
    }
    Ok(IntentLineageWriteOutcomeV1 {
        appended,
        execution_id: Some(execution_id.clone()),
    })
}

/// Appends advisory or system-verified criterion evidence after validating exact durable sources.
///
/// `SystemVerified` additionally requires an explicit CheckSpec Intent scope, current parent
/// snapshot, successful receipt, policy digest and exact execution ChangeSets.
///
/// # Errors
///
/// Returns an error when evidence is forged, stale at append time, or conflicts with prior facts.
pub fn append_intent_verification_evidence(
    session: &Session,
    evidence: Vec<IntentCriterionEvidenceV1>,
) -> Result<IntentLineageWriteOutcomeV1> {
    if evidence.is_empty() {
        bail!("Intent verification link requires evidence");
    }
    let store = session
        .durable_store()
        .context("Intent verification link requires a durable session")?;
    let records = JsonlSessionStore::read_event_records(store.path())?;
    let admission = IntentStackProjectionV1::from_records(&records)?;
    let projection = IntentLineageProjectionV1::from_records(&records, &admission)?;
    let facts = DurableLineageFacts::from_records(&records)?;
    validate_evidence_batch(&facts, &projection, &evidence, true)?;
    let event = IntentEventV1::VerificationLinked {
        schema_version: INTENT_CONTRACT_SCHEMA_VERSION,
        evidence: evidence.clone(),
    };
    let event_tuple = intent_event_tuple(DurableEventType::IntentVerificationLinked, &event)?;
    let predicate_evidence = evidence.clone();
    let appended = store
        .append_events_and_session_entries_if(vec![event_tuple], &[], move |records| {
            let admission = IntentStackProjectionV1::from_records(records)?;
            let projection = IntentLineageProjectionV1::from_records(records, &admission)?;
            if predicate_evidence
                .iter()
                .all(|item| projection.evidence.contains(item))
            {
                return Ok(false);
            }
            let facts = DurableLineageFacts::from_records(records)?;
            validate_evidence_batch(&facts, &projection, &predicate_evidence, true)?;
            Ok(true)
        })?
        .is_some();
    Ok(IntentLineageWriteOutcomeV1 {
        appended,
        execution_id: None,
    })
}

fn validate_evidence_batch(
    facts: &DurableLineageFacts,
    projection: &IntentLineageProjectionV1,
    evidence: &[IntentCriterionEvidenceV1],
    require_current: bool,
) -> Result<()> {
    for item in evidence {
        facts.validate_basic_evidence(item)?;
        let execution = projection
            .latest_execution_for(&item.intent_ref)
            .context("Intent verification evidence has no execution binding")?;
        let evidence_changesets = item
            .changeset_ids
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let execution_changesets = execution
            .changeset_ids
            .iter()
            .map(ChangeSetId::as_str)
            .collect::<BTreeSet<_>>();
        if evidence_changesets != execution_changesets {
            bail!("Intent verification evidence does not match execution ChangeSets");
        }
        if item.level == IntentCriterionEvidenceLevel::SystemVerified {
            facts.validate_system_evidence(item, execution)?;
            if require_current
                && projection.current_parent_snapshot_id.as_deref()
                    != Some(item.parent_snapshot_id.as_str())
            {
                bail!("system Intent evidence receipt is stale for the parent workspace");
            }
        }
    }
    Ok(())
}

fn direct_mutation_changeset(
    execution_id: &IntentExecutionId,
    prepared: &EventFact<MutationPrepared>,
    committed: &EventFact<MutationCommitted>,
    workspace_id: &str,
    execution_sequence: u64,
    attempt_window_end: Option<u64>,
) -> Result<(ChangeSet, ChangeSetResult)> {
    if prepared.stream_sequence <= execution_sequence
        || attempt_window_end.is_some_and(|window_end| {
            prepared.stream_sequence >= window_end || committed.stream_sequence >= window_end
        })
        || committed.stream_sequence <= prepared.stream_sequence
        || prepared.value.operation_id != committed.value.operation_id
        || prepared.value.workspace_id != workspace_id
        || committed.value.workspace_id.as_deref() != Some(workspace_id)
        || prepared.value.batch_id != committed.value.batch_id
        || committed.value.workspace_revision <= prepared.value.base_workspace_revision
        || prepared.value.subject != committed.value.committed_subject
        || prepared.value.intended_after_hash != committed.value.observed_after_hash
    {
        bail!("Chat mutation prepare/commit lineage is inconsistent");
    }
    let MutationSubject::File { path, .. } = &prepared.value.subject else {
        bail!("Chat direct ChangeSet only supports controlled file mutations");
    };
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        bail!("Chat direct ChangeSet requires a normalized workspace-relative file path");
    }
    let path = crate::mutation::portable_relative_path(path)
        .context("Chat direct ChangeSet path is not valid UTF-8")?;
    let action = match (
        prepared.value.before_hash.as_ref(),
        committed.value.observed_after_hash.as_ref(),
    ) {
        (None, Some(_)) => ChangeSetFileAction::Create,
        (Some(_), Some(_)) => ChangeSetFileAction::Update,
        (Some(_), None) => ChangeSetFileAction::Delete,
        (None, None) => bail!("Chat mutation has neither before nor after file content"),
    };
    let id =
        deterministic_changeset_id(execution_id, &prepared.event_id, &committed.event_id, &path)?;
    let file = ChangeSetFile {
        path: path.clone(),
        previous_path: None,
        action,
        risk: ChangeSetRisk::Low,
        before_hash: prepared.value.before_hash.clone(),
        after_hash: committed.value.observed_after_hash.clone(),
        diff_hash: None,
        additions: 0,
        deletions: 0,
        validations: vec![ChangeSetValidation {
            kind: ChangeSetValidationKind::Hash,
            status: ChangeSetValidationStatus::Passed,
            message: Some("projected from controlled mutation evidence".to_owned()),
        }],
    };
    Ok((
        ChangeSet {
            id: id.clone(),
            title: "Controlled Chat file mutation".to_owned(),
            summary: "Runtime-projected from exact mutation prepare and commit events.".to_owned(),
            risk: ChangeSetRisk::Low,
            files: vec![file],
            validations: Vec::new(),
        },
        ChangeSetResult {
            id,
            status: ChangeSetResultStatus::Applied,
            file_results: vec![ChangeSetFileResult {
                path,
                action,
                status: ChangeSetFileResultStatus::Applied,
                message: Some("confirmed by controlled mutation commit".to_owned()),
                validations: Vec::new(),
            }],
            message: Some("runtime-projected controlled mutation".to_owned()),
        },
    ))
}

fn deterministic_execution_id(
    accepted: &crate::AcceptedIntentPlanProjectionV1,
    intent_ref: &IntentVersionRef,
    origin: &IntentExecutionOriginV1,
    source_event_id: &str,
) -> Result<IntentExecutionId> {
    let material = serde_json::to_vec(&(
        accepted.plan.stack_id.as_str(),
        accepted.plan.stack_version.get(),
        intent_ref,
        origin,
        source_event_id,
    ))
    .context("failed to encode Intent execution identity")?;
    let digest = Sha256::digest(material);
    IntentExecutionId::new(format!("execution-{digest:x}")[..34].to_owned())
}

fn deterministic_changeset_id(
    execution_id: &IntentExecutionId,
    prepared_event_id: &str,
    committed_event_id: &str,
    path: &str,
) -> Result<ChangeSetId> {
    let mut digest = Sha256::new();
    for part in [
        execution_id.as_str(),
        prepared_event_id,
        committed_event_id,
        path,
    ] {
        digest.update(part.as_bytes());
        digest.update([0]);
    }
    let value = format!("intent-chat-{:x}", digest.finalize());
    ChangeSetId::new(value[..36].to_owned())
}

fn intent_event_tuple(
    event_type: DurableEventType,
    event: &IntentEventV1,
) -> Result<(DurableEventType, EventClass, Value)> {
    event.validate_contract()?;
    Ok((
        event_type,
        EventClass::Critical,
        serde_json::to_value(event).context("failed to serialize R51.2 Intent event")?,
    ))
}

fn durable_records(session: &Session) -> Result<Vec<SessionStreamRecord>> {
    let store = session
        .durable_store()
        .context("Intent lineage requires a durable session")?;
    JsonlSessionStore::read_event_records(store.path())
}

impl Session {
    /// Rebuilds the accepted Intent Stack together with R51.2 execution lineage.
    pub fn intent_lineage_projection(&self) -> Result<IntentLineageProjectionV1> {
        let records = durable_records(self)?;
        let admission = IntentStackProjectionV1::from_records(&records)?;
        IntentLineageProjectionV1::from_records(&records, &admission)
    }
}

#[cfg(test)]
#[path = "tests/intent_lineage_tests.rs"]
mod tests;
