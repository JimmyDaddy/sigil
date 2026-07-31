use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    ArtifactId, ControlEntry, IntentEventV1, IntentLayerProjectionV1, IntentLineageProjectionV1,
    IntentOperationProjectionV1, IntentPlanV1, IntentSourceV1, IntentStackProjectionV1,
    IntentVersionRef, MessageRole, ModelMessage, SessionLogEntry, TaskMemoryV1, TaskStepStatus,
    ToolExecutionStatus, TypedDomainEvent, TypedStoredEventDecode, decode_typed_stored_event,
    task_memory::ActiveTaskPlanV1,
};

/// Schema version for authority-only session anchors.
pub const SESSION_ANCHOR_V1_SCHEMA_VERSION: u16 = 1;
/// Schema version for source-bound portable continuity snapshots.
pub const CONVERSATION_CONTINUITY_V2_SCHEMA_VERSION: u16 = 2;
const MAX_ANCHOR_STATEMENTS: usize = 256;
const MAX_CONTINUITY_SECTION_ITEMS: usize = 256;
const MAX_CONTINUITY_TEXT_BYTES: usize = 16 * 1024;
const MAX_USER_OBJECTIVE_SPAN_BYTES: usize = 4 * 1024;
const WHOLE_EVENT_SOURCE_PATH: &str = "$event";

/// Bounded authority/evidence reference into the durable session stream.
///
/// JSON-pointer paths bind an exact durable string value. `$event` binds only the owning
/// append-only record and is reserved for explicitly unverified model narrative.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SourceSpanRefV1 {
    pub session_id: crate::SessionId,
    pub stream_sequence: u64,
    pub event_id: crate::EventId,
    /// Checksum of the exact append-only record that owns the cited field.
    pub record_checksum: String,
    pub field_path: String,
    pub byte_start: u64,
    pub byte_end: u64,
    /// SHA-256 of the exact projected value, or of `record_checksum` for `$event` references.
    pub cited_value_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

impl SourceSpanRefV1 {
    pub(super) fn from_record(
        record: &SessionStreamRecord,
        field_path: String,
        cited_value: &[u8],
        message_id: Option<String>,
    ) -> Result<Self> {
        let source = Self {
            session_id: record.session_id().to_owned(),
            stream_sequence: record.stream_sequence(),
            event_id: record.event_id().to_owned(),
            record_checksum: record.record_checksum().to_owned(),
            field_path,
            byte_start: 0,
            byte_end: u64::try_from(cited_value.len())
                .context("continuity source span length overflow")?,
            cited_value_hash: cited_value_hash(cited_value),
            message_id,
        };
        source.validate_shape()?;
        Ok(source)
    }

    pub(super) fn from_whole_event(
        record: &SessionStreamRecord,
        message_id: Option<String>,
    ) -> Result<Self> {
        let cited_value = record.record_checksum().as_bytes();
        Self::from_record(
            record,
            WHOLE_EVENT_SOURCE_PATH.to_owned(),
            cited_value,
            message_id,
        )
    }

    fn validate_shape(&self) -> Result<()> {
        if self.session_id.trim().is_empty()
            || self.stream_sequence == 0
            || self.event_id.trim().is_empty()
            || self.record_checksum.trim().is_empty()
            || self.field_path.trim().is_empty()
            || self.byte_end < self.byte_start
            || !is_sha256_digest(&self.cited_value_hash)
            || self
                .message_id
                .as_deref()
                .is_some_and(|message_id| message_id.trim().is_empty())
        {
            bail!("continuity source span is invalid");
        }
        Ok(())
    }

    fn validate_against_records(&self, records: &[SessionStreamRecord]) -> Result<()> {
        self.validate_shape()?;
        let record = records
            .iter()
            .find(|record| record.event_id() == self.event_id)
            .context("continuity source span references an unknown durable event")?;
        if record.session_id() != self.session_id
            || record.stream_sequence() != self.stream_sequence
            || record.record_checksum() != self.record_checksum
        {
            bail!("continuity source span does not match its durable event");
        }
        if self.field_path == WHOLE_EVENT_SOURCE_PATH
            && (self.byte_start != 0
                || self.byte_end != u64::try_from(self.record_checksum.len())?
                || self.cited_value_hash != cited_value_hash(self.record_checksum.as_bytes()))
        {
            bail!("continuity whole-event reference does not match its durable checksum");
        }
        Ok(())
    }

    fn exact_cited_string(&self, records: &[SessionStreamRecord]) -> Result<Option<String>> {
        self.validate_against_records(records)?;
        if !self.field_path.starts_with('/') {
            return Ok(None);
        }
        let record = records
            .iter()
            .find(|record| record.event_id() == self.event_id)
            .context("continuity source span references an unknown durable event")?;
        let stored = serde_json::to_value(record.stored_event())?;
        let value = stored
            .pointer(&self.field_path)
            .context("continuity source span references an unknown durable field")?
            .as_str()
            .context("continuity source span does not reference a durable string field")?;
        if self.byte_start != 0
            || self.byte_end != u64::try_from(value.len())?
            || self.cited_value_hash != cited_value_hash(value.as_bytes())
        {
            bail!("continuity source span does not match its exact durable field value");
        }
        Ok(Some(value.to_owned()))
    }
}

/// Existing durable authority from which an anchored statement was projected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObjectiveAuthorityRefV1 {
    AcceptedIntent {
        intent_ref: IntentVersionRef,
    },
    DurableTask {
        task_id: String,
        plan_version: u32,
    },
    UserSourceTurn {
        event_id: crate::EventId,
        message_id: String,
    },
}

/// Exact authority-bearing statement retained across compaction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct AnchoredStatementV1 {
    pub exact_text: String,
    pub authority: ObjectiveAuthorityRefV1,
    pub source: SourceSpanRefV1,
}

impl AnchoredStatementV1 {
    fn validate_shape(&self) -> Result<()> {
        if self.exact_text.trim().is_empty() || self.exact_text.len() > MAX_CONTINUITY_TEXT_BYTES {
            bail!("anchored statement text is invalid or exceeds its bounded size");
        }
        if self.source.cited_value_hash != cited_value_hash(self.exact_text.as_bytes()) {
            bail!("anchored statement text does not match its cited source value");
        }
        match &self.authority {
            ObjectiveAuthorityRefV1::AcceptedIntent { intent_ref } => intent_ref.validate()?,
            ObjectiveAuthorityRefV1::DurableTask {
                task_id,
                plan_version,
            } => {
                if task_id.trim().is_empty() || *plan_version == 0 {
                    bail!("anchored task authority is invalid");
                }
            }
            ObjectiveAuthorityRefV1::UserSourceTurn {
                event_id,
                message_id,
            } => {
                if event_id.trim().is_empty() || message_id.trim().is_empty() {
                    bail!("anchored user source turn is invalid");
                }
            }
        }
        self.source.validate_shape()
    }

    fn validate_against_records(&self, records: &[SessionStreamRecord]) -> Result<()> {
        self.validate_shape()?;
        self.source.validate_against_records(records)?;
        let record = records
            .iter()
            .find(|record| record.event_id() == self.source.event_id)
            .context("anchored statement references an unknown source record")?;
        match &self.authority {
            ObjectiveAuthorityRefV1::AcceptedIntent { intent_ref } => {
                let TypedStoredEventDecode::Known(event) =
                    decode_typed_stored_event(record.stored_event().clone())?
                else {
                    bail!("anchored Intent statement references an unknown typed event");
                };
                let TypedDomainEvent::Intent(IntentEventV1::PlanRecorded { plan, .. }) = *event
                else {
                    bail!("anchored Intent statement does not reference a recorded plan");
                };
                let (intent_index, intent) = plan
                    .intents
                    .iter()
                    .enumerate()
                    .find(|(_, intent)| &intent.intent_ref == intent_ref)
                    .context("anchored Intent statement is absent from its recorded plan")?;
                let objective_path = format!("plan.intents[{intent_index}].statement");
                let objective_matches =
                    self.source.field_path == objective_path && self.exact_text == intent.statement;
                let criterion_matches =
                    intent
                        .acceptance_criteria
                        .iter()
                        .enumerate()
                        .any(|(criterion_index, criterion)| {
                            self.source.field_path
                                == format!(
                                    "plan.intents[{intent_index}].acceptance_criteria[{criterion_index}].statement"
                                )
                                && self.exact_text == criterion.statement
                        });
                if !objective_matches && !criterion_matches {
                    bail!("anchored Intent statement does not match its exact durable field");
                }
            }
            ObjectiveAuthorityRefV1::DurableTask {
                task_id,
                plan_version,
            } => {
                let entry = session_entry(record)?
                    .context("anchored task statement does not reference a session entry")?;
                let matches = match entry {
                    SessionLogEntry::Control(ControlEntry::TaskPlan(plan)) => {
                        plan.task_id.as_str() == task_id
                            && plan.plan_version == *plan_version
                            && self.source.field_path
                                == "session_log_entry.control.task_plan.steps.title"
                            && plan.steps.iter().any(|step| step.title == self.exact_text)
                    }
                    SessionLogEntry::Control(ControlEntry::PlanPermissionGranted(grant)) => {
                        let exact_text = format!(
                            "plan permission {} with scope {} and expiry {}",
                            serde_json::to_string(&grant.permission)?,
                            serde_json::to_string(&grant.scope)?,
                            serde_json::to_string(&grant.expires)?
                        );
                        grant.task_id.as_str() == task_id
                            && self.source.field_path
                                == "session_log_entry.control.plan_permission_granted"
                            && exact_text == self.exact_text
                            && task_plan_version_for_grant(
                                records,
                                grant.plan_id.as_str(),
                                &grant.plan_hash,
                                grant.task_id.as_str(),
                            )? == Some(*plan_version)
                    }
                    _ => false,
                };
                if !matches {
                    bail!("anchored task statement does not match its exact durable field");
                }
            }
            ObjectiveAuthorityRefV1::UserSourceTurn {
                event_id,
                message_id,
            } => {
                let Some(SessionLogEntry::User(message)) = session_entry(record)? else {
                    bail!("anchored statement does not reference a durable user turn");
                };
                let content = message
                    .content
                    .as_deref()
                    .context("anchored user turn has no text")?;
                if event_id != record.event_id()
                    || message_id != &message.id
                    || self.source.message_id.as_deref() != Some(message.id.as_str())
                    || self.source.field_path != "session_log_entry.user.content"
                    || self.source.byte_start != 0
                    || self.source.byte_end != u64::try_from(self.exact_text.len())?
                    || !content.starts_with(&self.exact_text)
                {
                    bail!("anchored statement does not match its exact durable text span");
                }
            }
        }
        Ok(())
    }
}

/// Lifecycle state of a durable constraint projection.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ConstraintStatusV1 {
    Active,
    Superseded,
    Satisfied,
    Revoked,
}

/// One accepted constraint. Its status is derived from durable Intent lineage or an explicit
/// durable operation; model narrative can never create or mutate it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ActiveConstraintV1 {
    pub constraint_id: String,
    pub exact_text: String,
    pub authority: ObjectiveAuthorityRefV1,
    pub source: SourceSpanRefV1,
    pub status: ConstraintStatusV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
}

impl ActiveConstraintV1 {
    fn validate_shape(&self) -> Result<()> {
        if self.constraint_id.trim().is_empty()
            || self.exact_text.trim().is_empty()
            || self.exact_text.len() > MAX_CONTINUITY_TEXT_BYTES
        {
            bail!("anchored constraint is invalid or exceeds its bounded size");
        }
        AnchoredStatementV1 {
            exact_text: self.exact_text.clone(),
            authority: self.authority.clone(),
            source: self.source.clone(),
        }
        .validate_shape()?;
        if self
            .supersedes
            .iter()
            .any(|constraint_id| constraint_id.trim().is_empty())
        {
            bail!("anchored constraint has an empty superseded id");
        }
        Ok(())
    }
}

/// Recoverable durable attachment metadata retained by the authority projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DurableArtifactRefV1 {
    pub artifact_id: ArtifactId,
    pub content_hash: String,
    pub media_type: String,
    pub byte_size: u64,
    pub retrieval_ref: String,
    pub source: SourceSpanRefV1,
}

impl DurableArtifactRefV1 {
    fn validate_shape(&self) -> Result<()> {
        if self.artifact_id.trim().is_empty()
            || self.content_hash.trim().is_empty()
            || self.media_type.trim().is_empty()
            || self.retrieval_ref.trim().is_empty()
        {
            bail!("anchored artifact reference is invalid");
        }
        self.source.validate_shape()
    }
}

/// Complete authority projection used by V3 continuity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SessionAnchorV1 {
    pub schema_version: u16,
    pub root_objective: AnchoredStatementV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_subgoal: Option<AnchoredStatementV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ActiveConstraintV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authorization_boundary: Vec<ActiveConstraintV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_refs: Vec<DurableArtifactRefV1>,
    pub source_cursor: ProjectionCursor,
    pub canonical_hash: String,
}

impl SessionAnchorV1 {
    /// Derives an anchor from accepted Intent/Task/control state or the current conversation's
    /// exact source turn when no accepted Intent facts exist.
    pub fn derive(
        records: &[SessionStreamRecord],
        memory: &TaskMemoryV1,
        at_unix_ms: u64,
    ) -> Result<Self> {
        if at_unix_ms == 0 {
            bail!("session anchor derivation time must be non-zero");
        }
        let source_cursor = records
            .last()
            .context("cannot derive a session anchor from an empty stream")?
            .projection_cursor(SESSION_ANCHOR_V1_SCHEMA_VERSION);
        let intent_projection = IntentStackProjectionV1::from_records(records)?;
        let lineage_projection =
            IntentLineageProjectionV1::from_records(records, &intent_projection)?;
        let layer_projection = IntentLayerProjectionV1::from_records(
            records,
            &intent_projection,
            &lineage_projection,
        )?;
        let operation_projection = IntentOperationProjectionV1::from_records(
            records,
            &intent_projection,
            &layer_projection,
        )?;
        let recorded_plans = recorded_intent_plans(records)?;

        let (root_objective, constraints, accepted_source_turns) =
            if let Some(accepted) = intent_projection.latest_accepted_plan() {
                let (plan_record, plan) = recorded_plans
                    .iter()
                    .find(|(_, plan)| {
                        plan.stack_id == accepted.plan.stack_id
                            && plan.stack_version == accepted.plan.stack_version
                    })
                    .context("accepted Intent plan has no durable recorded source")?;
                let root_index = plan
                    .intents
                    .iter()
                    .position(|intent| intent.depends_on.is_empty())
                    .unwrap_or(0);
                let root = &plan.intents[root_index];
                let root_source = source_span_for_event(
                    plan_record,
                    format!("plan.intents[{root_index}].statement"),
                    root.statement.as_bytes(),
                    intent_source_turn_id(&root.source).map(str::to_owned),
                )?;
                let root_objective = AnchoredStatementV1 {
                    exact_text: root.statement.clone(),
                    authority: ObjectiveAuthorityRefV1::AcceptedIntent {
                        intent_ref: root.intent_ref.clone(),
                    },
                    source: root_source,
                };
                let constraints = derive_intent_constraints(
                    records,
                    &recorded_plans,
                    plan_record,
                    plan,
                    &operation_projection,
                )?;
                let source_turns = plan
                    .intents
                    .iter()
                    .filter_map(|intent| intent_source_turn_id(&intent.source))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                (root_objective, constraints, source_turns)
            } else {
                let (record, message) = first_durable_user_message(records)?
                    .context("session has no durable user source turn")?;
                let full_text = message
                    .content
                    .clone()
                    .filter(|text| !text.trim().is_empty())
                    .context("root user turn has no text")?;
                let exact_text = bounded_user_objective_span(&full_text);
                let source = source_span_for_event(
                    record,
                    "session_log_entry.user.content".to_owned(),
                    exact_text.as_bytes(),
                    Some(message.id.clone()),
                )?;
                (
                    AnchoredStatementV1 {
                        exact_text,
                        authority: ObjectiveAuthorityRefV1::UserSourceTurn {
                            event_id: record.event_id().to_owned(),
                            message_id: message.id.clone(),
                        },
                        source,
                    },
                    Vec::new(),
                    BTreeSet::from([message.id]),
                )
            };

        let active_subgoal = derive_active_subgoal(records, memory.active_plan.as_ref())?;
        let authorization_boundary = derive_authorization_boundary(records, at_unix_ms)?;
        let attachment_refs = derive_attachment_refs(records, &accepted_source_turns)?;
        let mut anchor = Self {
            schema_version: SESSION_ANCHOR_V1_SCHEMA_VERSION,
            root_objective,
            active_subgoal,
            constraints,
            authorization_boundary,
            attachment_refs,
            source_cursor,
            canonical_hash: String::new(),
        };
        anchor.canonical_hash = anchor.computed_hash()?;
        anchor.validate_against_records(records)?;
        Ok(anchor)
    }

    #[must_use]
    pub fn uses_user_turn_authority(&self) -> bool {
        matches!(
            self.root_objective.authority,
            ObjectiveAuthorityRefV1::UserSourceTurn { .. }
        )
    }

    /// Validates identity, source scope and the self-verifying canonical hash.
    pub fn validate_against_records(&self, records: &[SessionStreamRecord]) -> Result<()> {
        self.validate_shape()?;
        validate_cursor_against_records(&self.source_cursor, records)?;
        self.root_objective.validate_against_records(records)?;
        if let Some(subgoal) = &self.active_subgoal {
            subgoal.validate_against_records(records)?;
        }
        for constraint in self.constraints.iter().chain(&self.authorization_boundary) {
            AnchoredStatementV1 {
                exact_text: constraint.exact_text.clone(),
                authority: constraint.authority.clone(),
                source: constraint.source.clone(),
            }
            .validate_against_records(records)?;
        }
        for attachment in &self.attachment_refs {
            attachment.source.validate_against_records(records)?;
        }
        Ok(())
    }

    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != SESSION_ANCHOR_V1_SCHEMA_VERSION
            || self.constraints.len() > MAX_ANCHOR_STATEMENTS
            || self.authorization_boundary.len() > MAX_ANCHOR_STATEMENTS
        {
            bail!("unsupported or unbounded session anchor");
        }
        self.root_objective.validate_shape()?;
        if let Some(subgoal) = &self.active_subgoal {
            subgoal.validate_shape()?;
        }
        let mut constraint_ids = BTreeSet::new();
        for constraint in self.constraints.iter().chain(&self.authorization_boundary) {
            constraint.validate_shape()?;
            if !constraint_ids.insert(constraint.constraint_id.as_str()) {
                bail!("session anchor contains duplicate constraint ids");
            }
        }
        for attachment in &self.attachment_refs {
            attachment.validate_shape()?;
        }
        if self.canonical_hash != self.computed_hash()? {
            bail!("session anchor canonical hash mismatch");
        }
        Ok(())
    }

    fn computed_hash(&self) -> Result<String> {
        let mut payload = self.clone();
        payload.canonical_hash.clear();
        crate::event::canonical_json_content_hash(&serde_json::to_value(payload)?)
    }
}

/// Stable reference from continuity to its authority projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct SessionAnchorRefV1 {
    pub anchor_id: String,
    pub canonical_hash: String,
}

/// One source-grounded continuity item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct GroundedContinuityItemV2 {
    pub text: String,
    pub source_refs: Vec<SourceSpanRefV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt_ref: Option<String>,
}

impl GroundedContinuityItemV2 {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.text.trim().is_empty()
            || self.text.len() > MAX_CONTINUITY_TEXT_BYTES
            || self.source_refs.is_empty()
            || self
                .artifact_ref
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
            || self
                .receipt_ref
                .as_deref()
                .is_some_and(|value| value.trim().is_empty())
        {
            bail!("grounded continuity item is invalid");
        }
        let mut unique = BTreeSet::new();
        for source in &self.source_refs {
            source.validate_shape()?;
            if !unique.insert(source) {
                bail!("grounded continuity item contains duplicate sources");
            }
        }
        Ok(())
    }

    fn validate_against_records(
        &self,
        records: &[SessionStreamRecord],
        require_exact_text_binding: bool,
    ) -> Result<()> {
        self.validate_shape()?;
        let mut exact_values = BTreeSet::new();
        for source in &self.source_refs {
            if require_exact_text_binding {
                let value = source
                    .exact_cited_string(records)?
                    .context("grounded continuity source is not an exact durable field citation")?;
                exact_values.insert(value);
            } else {
                source.validate_against_records(records)?;
            }
        }
        if require_exact_text_binding {
            if !exact_values.contains(&self.text) {
                bail!("grounded continuity text does not match an exact cited durable field");
            }
            if self
                .artifact_ref
                .as_ref()
                .is_some_and(|artifact| !exact_values.contains(artifact))
                || self
                    .receipt_ref
                    .as_ref()
                    .is_some_and(|receipt| !exact_values.contains(receipt))
            {
                bail!("grounded continuity reference does not match an exact cited durable field");
            }
        }
        Ok(())
    }
}

/// Model narrative may help rendering but is explicitly untrusted and source-selected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct UntrustedModelNarrativeV2 {
    pub items: Vec<GroundedContinuityItemV2>,
}

/// Complete source-bound portable continuity snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ConversationContinuityV2 {
    pub schema_version: u16,
    pub checkpoint_id: String,
    pub source_cursor: ProjectionCursor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_checkpoint_id: Option<String>,
    pub anchor_ref: SessionAnchorRefV1,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub progress: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_work: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_and_artifacts: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failures_and_dead_ends: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_questions: Vec<GroundedContinuityItemV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narrative: Option<UntrustedModelNarrativeV2>,
}

impl ConversationContinuityV2 {
    /// Builds a full snapshot from current durable projections, not by recursively trusting the
    /// previous rendered summary.
    pub fn derive(
        records: &[SessionStreamRecord],
        memory: &TaskMemoryV1,
        anchor: &SessionAnchorV1,
        previous_checkpoint_id: Option<String>,
        narrative_items: Vec<GroundedContinuityItemV2>,
    ) -> Result<Self> {
        anchor.validate_against_records(records)?;
        let source_cursor = records
            .last()
            .context("cannot derive continuity from an empty stream")?
            .projection_cursor(CONVERSATION_CONTINUITY_V2_SCHEMA_VERSION);
        let mut continuity = Self {
            schema_version: CONVERSATION_CONTINUITY_V2_SCHEMA_VERSION,
            checkpoint_id: String::new(),
            source_cursor,
            previous_checkpoint_id,
            anchor_ref: SessionAnchorRefV1 {
                anchor_id: format!("session-anchor:{}", anchor.canonical_hash),
                canonical_hash: anchor.canonical_hash.clone(),
            },
            decisions: sourced_fact_items(records, &memory.decisions, |decision| {
                &decision.decision
            })?,
            progress: progress_items(records, memory)?,
            pending_work: pending_work_items(records, memory)?,
            files_and_artifacts: file_and_artifact_items(records, memory, anchor)?,
            commands: command_items(records)?,
            verification: verification_items(records)?,
            failures_and_dead_ends: failure_items(records, memory)?,
            risks: sourced_plain_fact_items(records, &memory.risks)?,
            unresolved_questions: sourced_plain_fact_items(records, &memory.unresolved_issues)?,
            narrative: (!narrative_items.is_empty()).then_some(UntrustedModelNarrativeV2 {
                items: narrative_items,
            }),
        };
        let content_hash = continuity.computed_hash()?;
        continuity.checkpoint_id = format!("continuity-v2:{content_hash}");
        continuity.validate_against_records(records, anchor)?;
        Ok(continuity)
    }

    /// Validates source scope, lineage, bounded sections and checkpoint identity.
    pub fn validate_against_records(
        &self,
        records: &[SessionStreamRecord],
        anchor: &SessionAnchorV1,
    ) -> Result<()> {
        self.validate_shape(anchor)?;
        validate_cursor_against_records(&self.source_cursor, records)?;
        for section in self.sections() {
            for item in section {
                item.validate_against_records(records, true)?;
            }
        }
        if let Some(narrative) = &self.narrative {
            for item in &narrative.items {
                item.validate_against_records(records, false)?;
            }
        }
        Ok(())
    }

    pub(crate) fn validate_shape(&self, anchor: &SessionAnchorV1) -> Result<()> {
        if self.schema_version != CONVERSATION_CONTINUITY_V2_SCHEMA_VERSION
            || self.checkpoint_id.trim().is_empty()
            || self.anchor_ref.canonical_hash != anchor.canonical_hash
            || self.anchor_ref.anchor_id != format!("session-anchor:{}", anchor.canonical_hash)
            || self
                .previous_checkpoint_id
                .as_deref()
                .is_some_and(|id| id.trim().is_empty() || id == self.checkpoint_id)
        {
            bail!("conversation continuity identity is invalid");
        }
        for section in self.sections() {
            if section.len() > MAX_CONTINUITY_SECTION_ITEMS {
                bail!("conversation continuity section exceeds its item limit");
            }
            for item in section {
                item.validate_shape()?;
            }
        }
        if let Some(narrative) = &self.narrative {
            if narrative.items.len() > MAX_CONTINUITY_SECTION_ITEMS {
                bail!("conversation continuity narrative exceeds its item limit");
            }
            for item in &narrative.items {
                item.validate_shape()?;
            }
        }
        let expected = format!("continuity-v2:{}", self.computed_hash()?);
        if self.checkpoint_id != expected {
            bail!("conversation continuity checkpoint id mismatch");
        }
        Ok(())
    }

    fn sections(&self) -> [&[GroundedContinuityItemV2]; 9] {
        [
            &self.decisions,
            &self.progress,
            &self.pending_work,
            &self.files_and_artifacts,
            &self.commands,
            &self.verification,
            &self.failures_and_dead_ends,
            &self.risks,
            &self.unresolved_questions,
        ]
    }

    fn computed_hash(&self) -> Result<String> {
        let mut payload = self.clone();
        payload.checkpoint_id.clear();
        crate::event::canonical_json_content_hash(&serde_json::to_value(payload)?)
    }
}

fn recorded_intent_plans(
    records: &[SessionStreamRecord],
) -> Result<Vec<(&SessionStreamRecord, IntentPlanV1)>> {
    let mut plans = Vec::new();
    for record in records {
        if let TypedStoredEventDecode::Known(event) =
            decode_typed_stored_event(record.stored_event().clone())?
            && let TypedDomainEvent::Intent(IntentEventV1::PlanRecorded { plan, .. }) = *event
        {
            plans.push((record, plan));
        }
    }
    Ok(plans)
}

fn derive_intent_constraints(
    records: &[SessionStreamRecord],
    recorded_plans: &[(&SessionStreamRecord, IntentPlanV1)],
    current_record: &SessionStreamRecord,
    current_plan: &IntentPlanV1,
    operations: &IntentOperationProjectionV1,
) -> Result<Vec<ActiveConstraintV1>> {
    let mut constraints = Vec::new();
    let mut superseded_by_intent = BTreeMap::<IntentVersionRef, IntentVersionRef>::new();
    for (_, plan) in recorded_plans {
        for intent in &plan.intents {
            if let Some(previous) = &intent.supersedes {
                superseded_by_intent.insert(previous.clone(), intent.intent_ref.clone());
            }
        }
    }

    for (record, plan) in recorded_plans {
        for (intent_index, intent) in plan.intents.iter().enumerate() {
            let current = current_plan
                .intents
                .iter()
                .find(|candidate| candidate.intent_ref == intent.intent_ref);
            let is_superseded_ancestor =
                intent_reaches_current(&intent.intent_ref, &superseded_by_intent, current_plan);
            if current.is_none() && !is_superseded_ancestor {
                continue;
            }
            let status = if is_superseded_ancestor {
                ConstraintStatusV1::Superseded
            } else if operations.is_dropped(&intent.intent_ref) {
                ConstraintStatusV1::Revoked
            } else {
                ConstraintStatusV1::Active
            };
            for (criterion_index, criterion) in intent
                .acceptance_criteria
                .iter()
                .enumerate()
                .filter(|(_, criterion)| criterion.required)
            {
                let current_constraint_id =
                    constraint_id(&intent.intent_ref, criterion.criterion_id.as_str());
                let supersedes = current
                    .and_then(|current| current.supersedes.as_ref())
                    .and_then(|previous_ref| {
                        recorded_plans
                            .iter()
                            .flat_map(|(_, plan)| &plan.intents)
                            .find(|candidate| &candidate.intent_ref == previous_ref)
                    })
                    .map(|previous| {
                        previous
                            .acceptance_criteria
                            .iter()
                            .filter(|candidate| candidate.required)
                            .map(|candidate| {
                                constraint_id(&previous.intent_ref, candidate.criterion_id.as_str())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let source_record = if plan.stack_version == current_plan.stack_version {
                    current_record
                } else {
                    *record
                };
                constraints.push(ActiveConstraintV1 {
                    constraint_id: current_constraint_id,
                    exact_text: criterion.statement.clone(),
                    authority: ObjectiveAuthorityRefV1::AcceptedIntent {
                        intent_ref: intent.intent_ref.clone(),
                    },
                    source: source_span_for_event(
                        source_record,
                        format!(
                            "plan.intents[{intent_index}].acceptance_criteria[{criterion_index}].statement"
                        ),
                        criterion.statement.as_bytes(),
                        intent_source_turn_id(&intent.source).map(str::to_owned),
                    )?,
                    status,
                    supersedes,
                });
            }
        }
    }
    constraints.sort_by(|left, right| left.constraint_id.cmp(&right.constraint_id));
    if constraints.len() > MAX_ANCHOR_STATEMENTS {
        bail!("accepted Intent constraints exceed the anchor limit");
    }
    for constraint in &constraints {
        constraint.source.validate_against_records(records)?;
    }
    Ok(constraints)
}

fn intent_reaches_current(
    candidate: &IntentVersionRef,
    replacements: &BTreeMap<IntentVersionRef, IntentVersionRef>,
    current_plan: &IntentPlanV1,
) -> bool {
    let mut cursor = candidate;
    let mut visited = BTreeSet::new();
    while let Some(replacement) = replacements.get(cursor) {
        if !visited.insert(cursor.clone()) {
            return false;
        }
        if current_plan
            .intents
            .iter()
            .any(|intent| intent.intent_ref == *replacement)
        {
            return true;
        }
        cursor = replacement;
    }
    false
}

fn constraint_id(intent_ref: &IntentVersionRef, criterion_id: &str) -> String {
    format!(
        "intent:{}:v{}:criterion:{criterion_id}",
        intent_ref.intent_id.as_str(),
        intent_ref.version
    )
}

fn derive_active_subgoal(
    records: &[SessionStreamRecord],
    plan: Option<&ActiveTaskPlanV1>,
) -> Result<Option<AnchoredStatementV1>> {
    let Some(plan) = plan else {
        return Ok(None);
    };
    let step = plan.steps.iter().find(|step| {
        matches!(
            step.status,
            TaskStepStatus::Running | TaskStepStatus::Pending | TaskStepStatus::Interrupted
        )
    });
    let Some(step) = step else {
        return Ok(None);
    };
    let record = records
        .iter()
        .find(|record| record.event_id() == plan.source_event_id)
        .context("active task plan references an unknown durable event")?;
    Ok(Some(AnchoredStatementV1 {
        exact_text: step.title.clone(),
        authority: ObjectiveAuthorityRefV1::DurableTask {
            task_id: plan.task_id.clone(),
            plan_version: plan.plan_version,
        },
        source: source_span_for_event(
            record,
            "session_log_entry.control.task_plan.steps.title".to_owned(),
            step.title.as_bytes(),
            None,
        )?,
    }))
}

fn derive_authorization_boundary(
    records: &[SessionStreamRecord],
    at_unix_ms: u64,
) -> Result<Vec<ActiveConstraintV1>> {
    let mut boundary = Vec::new();
    for (record_index, record) in records.iter().enumerate() {
        let Some(SessionLogEntry::Control(ControlEntry::PlanPermissionGranted(grant))) =
            session_entry(record)?
        else {
            continue;
        };
        if task_is_terminal_after(records, record_index, &grant.task_id)? {
            continue;
        }
        let active = match grant.expires {
            crate::PlanApprovalExpiry::NextUserPrompt => {
                !has_user_message_after(records, record_index)?
            }
            crate::PlanApprovalExpiry::Session => true,
            crate::PlanApprovalExpiry::AtUnixMs(expires_at_ms) => at_unix_ms <= expires_at_ms,
        };
        if !active {
            continue;
        }
        let Some(plan_version) = task_plan_version_for_grant(
            records,
            grant.plan_id.as_str(),
            &grant.plan_hash,
            grant.task_id.as_str(),
        )?
        else {
            continue;
        };
        let exact_text = format!(
            "plan permission {} with scope {} and expiry {}",
            serde_json::to_string(&grant.permission)?,
            serde_json::to_string(&grant.scope)?,
            serde_json::to_string(&grant.expires)?
        );
        boundary.push(ActiveConstraintV1 {
            constraint_id: format!(
                "plan-permission:{}:{}:{}",
                grant.plan_id.as_str(),
                grant.task_id.as_str(),
                record.stream_sequence()
            ),
            exact_text: exact_text.clone(),
            authority: ObjectiveAuthorityRefV1::DurableTask {
                task_id: grant.task_id.as_str().to_owned(),
                plan_version,
            },
            source: source_span_for_event(
                record,
                "session_log_entry.control.plan_permission_granted".to_owned(),
                exact_text.as_bytes(),
                None,
            )?,
            status: ConstraintStatusV1::Active,
            supersedes: Vec::new(),
        });
    }
    Ok(boundary)
}

fn task_is_terminal_after(
    records: &[SessionStreamRecord],
    record_index: usize,
    task_id: &crate::TaskId,
) -> Result<bool> {
    for record in records.iter().skip(record_index.saturating_add(1)) {
        if matches!(
            session_entry(record)?,
            Some(SessionLogEntry::Control(ControlEntry::TaskRun(run)))
                if &run.task_id == task_id && run.status.is_terminal()
        ) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_user_message_after(records: &[SessionStreamRecord], record_index: usize) -> Result<bool> {
    for record in records.iter().skip(record_index.saturating_add(1)) {
        if matches!(session_entry(record)?, Some(SessionLogEntry::User(_))) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn task_plan_version_for_grant(
    records: &[SessionStreamRecord],
    plan_id: &str,
    plan_hash: &str,
    task_id: &str,
) -> Result<Option<u32>> {
    for record in records {
        let Some(SessionLogEntry::Control(ControlEntry::TaskCreatedFromPlan(binding))) =
            session_entry(record)?
        else {
            continue;
        };
        if binding.plan_id.as_str() == plan_id
            && binding.plan_hash == plan_hash
            && binding.task_id.as_str() == task_id
        {
            return Ok(Some(binding.task_plan_version));
        }
    }
    Ok(None)
}

fn derive_attachment_refs(
    records: &[SessionStreamRecord],
    accepted_source_turns: &BTreeSet<String>,
) -> Result<Vec<DurableArtifactRefV1>> {
    let mut attachments = BTreeMap::new();
    for record in records {
        let Some(SessionLogEntry::User(message)) = session_entry(record)? else {
            continue;
        };
        if !accepted_source_turns.contains(&message.id) {
            continue;
        }
        for attachment in &message.image_attachments {
            attachment.validate()?;
            attachments
                .entry(attachment.attachment_id.clone())
                .or_insert(DurableArtifactRefV1 {
                    artifact_id: attachment.attachment_id.clone(),
                    content_hash: attachment.sha256.clone(),
                    media_type: attachment.mime_type.as_str().to_owned(),
                    byte_size: attachment.byte_len,
                    retrieval_ref: attachment.artifact_ref.clone(),
                    source: source_span_for_event(
                        record,
                        "session_log_entry.user.image_attachments".to_owned(),
                        attachment.sha256.as_bytes(),
                        Some(message.id.clone()),
                    )?,
                });
        }
        if let Some(content) = message
            .content
            .as_ref()
            .filter(|content| content.len() > MAX_CONTINUITY_TEXT_BYTES)
        {
            let artifact_id = format!("session-message-body:{}", message.id);
            attachments
                .entry(artifact_id.clone())
                .or_insert(DurableArtifactRefV1 {
                    artifact_id,
                    content_hash: crate::event::canonical_json_content_hash(
                        &serde_json::Value::String(content.clone()),
                    )?,
                    media_type: "text/plain; charset=utf-8".to_owned(),
                    byte_size: u64::try_from(content.len())
                        .context("durable user message length overflows u64")?,
                    retrieval_ref: format!(
                        "session-event:{}#session_log_entry.user.content",
                        record.event_id()
                    ),
                    source: source_span_for_event(
                        record,
                        "session_log_entry.user.content".to_owned(),
                        content.as_bytes(),
                        Some(message.id.clone()),
                    )?,
                });
        }
    }
    Ok(attachments.into_values().collect())
}

fn bounded_user_objective_span(content: &str) -> String {
    if content.len() <= MAX_USER_OBJECTIVE_SPAN_BYTES {
        return content.to_owned();
    }
    let mut end = MAX_USER_OBJECTIVE_SPAN_BYTES;
    while !content.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let prefix = &content[..end];
    prefix
        .split_once("\n\n")
        .map(|(first_paragraph, _)| first_paragraph)
        .filter(|first_paragraph| !first_paragraph.trim().is_empty())
        .unwrap_or(prefix)
        .to_owned()
}

fn first_durable_user_message(
    records: &[SessionStreamRecord],
) -> Result<Option<(&SessionStreamRecord, ModelMessage)>> {
    for record in records {
        if let Some(SessionLogEntry::User(message)) = session_entry(record)?
            && message.role == MessageRole::User
        {
            return Ok(Some((record, message)));
        }
    }
    Ok(None)
}

fn intent_source_turn_id(source: &IntentSourceV1) -> Option<&str> {
    match source {
        IntentSourceV1::UserTurn { source_turn_id }
        | IntentSourceV1::AcceptedSuggestion { source_turn_id, .. } => Some(source_turn_id),
        IntentSourceV1::TrustedSpec { .. } => None,
    }
}

fn session_entry(record: &SessionStreamRecord) -> Result<Option<SessionLogEntry>> {
    let Some(value) = record.stored_event().payload.get("session_log_entry") else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(value.clone())?))
}

fn source_span_for_event(
    record: &SessionStreamRecord,
    field_path: String,
    cited_value: &[u8],
    message_id: Option<String>,
) -> Result<SourceSpanRefV1> {
    SourceSpanRefV1::from_record(record, field_path, cited_value, message_id)
}

fn exact_string_source_for_event_id(
    records: &[SessionStreamRecord],
    event_id: &str,
    expected: &str,
) -> Result<Option<SourceSpanRefV1>> {
    let record = records
        .iter()
        .find(|record| record.event_id() == event_id)
        .with_context(|| format!("continuity fact references unknown event {event_id}"))?;
    let value = serde_json::to_value(record.stored_event())?;
    let mut pointers = Vec::new();
    collect_exact_string_pointers(&value, expected, String::new(), &mut pointers);
    pointers.retain(|pointer| pointer.starts_with("/payload/"));
    pointers.sort();
    let Some(pointer) = pointers.into_iter().next() else {
        return Ok(None);
    };
    Ok(Some(source_span_for_event(
        record,
        pointer,
        expected.as_bytes(),
        None,
    )?))
}

fn required_exact_string_source_for_event_id(
    records: &[SessionStreamRecord],
    event_id: &str,
    expected: &str,
) -> Result<SourceSpanRefV1> {
    exact_string_source_for_event_id(records, event_id, expected)?.with_context(|| {
        format!("continuity fact {expected:?} is not an exact durable field of event {event_id}")
    })
}

fn collect_exact_string_pointers(
    value: &serde_json::Value,
    expected: &str,
    pointer: String,
    output: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(current) if current == expected => output.push(pointer),
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_exact_string_pointers(
                    value,
                    expected,
                    format!("{pointer}/{index}"),
                    output,
                );
            }
        }
        serde_json::Value::Object(values) => {
            for (key, value) in values {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                collect_exact_string_pointers(
                    value,
                    expected,
                    format!("{pointer}/{escaped}"),
                    output,
                );
            }
        }
        _ => {}
    }
}

fn cited_value_hash(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn validate_cursor_against_records(
    cursor: &ProjectionCursor,
    records: &[SessionStreamRecord],
) -> Result<()> {
    let record = records
        .iter()
        .find(|record| record.stream_sequence() == cursor.last_applied_stream_sequence)
        .context("continuity source cursor is outside the durable stream")?;
    if record.session_id() != cursor.session_id
        || record.event_id() != cursor.last_applied_event_id
        || record.record_checksum() != cursor.last_applied_record_checksum
    {
        bail!("continuity source cursor does not match its durable record");
    }
    Ok(())
}

fn sourced_fact_items<F>(
    records: &[SessionStreamRecord],
    decisions: &[crate::SourcedDecision],
    fact: F,
) -> Result<Vec<GroundedContinuityItemV2>>
where
    F: Fn(&crate::SourcedDecision) -> &crate::SourcedFact,
{
    sourced_plain_fact_items(
        records,
        &decisions.iter().map(fact).cloned().collect::<Vec<_>>(),
    )
}

fn sourced_plain_fact_items(
    records: &[SessionStreamRecord],
    facts: &[crate::SourcedFact],
) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    for fact in facts.iter().filter(|fact| !fact.model_generated) {
        let Some(event_id) = fact.source_event_id.as_deref() else {
            continue;
        };
        let Some(text_source) = exact_string_source_for_event_id(records, event_id, &fact.text)?
        else {
            continue;
        };
        let mut source_refs = vec![text_source];
        if let Some(artifact_ref) = &fact.source_artifact_id {
            source_refs.push(required_exact_string_source_for_event_id(
                records,
                event_id,
                artifact_ref,
            )?);
        }
        if let Some(receipt_ref) = &fact.source_receipt_id {
            source_refs.push(required_exact_string_source_for_event_id(
                records,
                event_id,
                receipt_ref,
            )?);
        }
        items.push(GroundedContinuityItemV2 {
            text: fact.text.clone(),
            source_refs,
            artifact_ref: fact.source_artifact_id.clone(),
            receipt_ref: fact.source_receipt_id.clone(),
        });
    }
    Ok(items)
}

fn progress_items(
    records: &[SessionStreamRecord],
    memory: &TaskMemoryV1,
) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    if let Some(plan) = &memory.active_plan {
        for step in plan
            .steps
            .iter()
            .filter(|step| step.status == TaskStepStatus::Completed)
        {
            items.push(GroundedContinuityItemV2 {
                source_refs: task_step_sources(records, plan, step)?,
                text: step.title.clone(),
                artifact_ref: None,
                receipt_ref: None,
            });
        }
    }
    Ok(items)
}

fn pending_work_items(
    records: &[SessionStreamRecord],
    memory: &TaskMemoryV1,
) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    if let Some(plan) = &memory.active_plan {
        for step in plan.steps.iter().filter(|step| {
            matches!(
                step.status,
                TaskStepStatus::Pending | TaskStepStatus::Running | TaskStepStatus::Interrupted
            )
        }) {
            items.push(GroundedContinuityItemV2 {
                source_refs: task_step_sources(records, plan, step)?,
                text: step.title.clone(),
                artifact_ref: None,
                receipt_ref: None,
            });
        }
    }
    Ok(items)
}

fn task_step_sources(
    records: &[SessionStreamRecord],
    plan: &ActiveTaskPlanV1,
    step: &crate::task_memory::ActiveTaskPlanStepV1,
) -> Result<Vec<SourceSpanRefV1>> {
    let mut sources = vec![required_exact_string_source_for_event_id(
        records,
        &plan.source_event_id,
        &step.title,
    )?];
    if step.source_event_id != plan.source_event_id {
        let status = serde_json::to_value(step.status)?
            .as_str()
            .context("task step status is not serialized as a string")?
            .to_owned();
        sources.push(required_exact_string_source_for_event_id(
            records,
            &step.source_event_id,
            &status,
        )?);
    }
    Ok(sources)
}

fn file_and_artifact_items(
    records: &[SessionStreamRecord],
    memory: &TaskMemoryV1,
    _anchor: &SessionAnchorV1,
) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    for file in &memory.files_changed {
        let Some(event_id) = file.source_event_id.as_deref() else {
            continue;
        };
        let text = file.path.to_string_lossy().into_owned();
        let mut source_refs = vec![required_exact_string_source_for_event_id(
            records, event_id, &text,
        )?];
        if let Some(receipt_ref) = &file.mutation_receipt_id {
            source_refs.push(required_exact_string_source_for_event_id(
                records,
                event_id,
                receipt_ref,
            )?);
        }
        items.push(GroundedContinuityItemV2 {
            source_refs,
            text,
            artifact_ref: None,
            receipt_ref: file.mutation_receipt_id.clone(),
        });
    }
    Ok(items)
}

fn command_items(records: &[SessionStreamRecord]) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    for record in records {
        let Some(SessionLogEntry::Control(ControlEntry::ToolExecution(execution))) =
            session_entry(record)?
        else {
            continue;
        };
        if execution.status != ToolExecutionStatus::Completed {
            continue;
        }
        let text = execution.tool_name.clone();
        items.push(GroundedContinuityItemV2 {
            source_refs: vec![
                required_exact_string_source_for_event_id(
                    records,
                    record.event_id(),
                    &execution.tool_name,
                )?,
                required_exact_string_source_for_event_id(
                    records,
                    record.event_id(),
                    &execution.call_id,
                )?,
            ],
            text,
            artifact_ref: None,
            receipt_ref: Some(execution.call_id),
        });
    }
    Ok(items)
}

fn verification_items(records: &[SessionStreamRecord]) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    for record in records {
        let Some(SessionLogEntry::Control(ControlEntry::VerificationRecorded(verification))) =
            session_entry(record)?
        else {
            continue;
        };
        let receipt = &verification.receipt;
        let text = receipt.check_spec_id.clone();
        items.push(GroundedContinuityItemV2 {
            source_refs: vec![
                required_exact_string_source_for_event_id(
                    records,
                    record.event_id(),
                    &receipt.check_spec_id,
                )?,
                required_exact_string_source_for_event_id(
                    records,
                    record.event_id(),
                    &receipt.receipt.receipt_id,
                )?,
            ],
            text,
            artifact_ref: None,
            receipt_ref: Some(receipt.receipt.receipt_id.clone()),
        });
    }
    Ok(items)
}

fn failure_items(
    records: &[SessionStreamRecord],
    memory: &TaskMemoryV1,
) -> Result<Vec<GroundedContinuityItemV2>> {
    let mut items = Vec::new();
    for failure in &memory.failed_attempts {
        let Some(event_id) = failure.source_event_id.as_deref() else {
            continue;
        };
        let text = failure
            .summary
            .clone()
            .unwrap_or_else(|| failure.attempt_id.clone());
        let mut source_refs = vec![required_exact_string_source_for_event_id(
            records, event_id, &text,
        )?];
        if text != failure.attempt_id {
            source_refs.push(required_exact_string_source_for_event_id(
                records,
                event_id,
                &failure.attempt_id,
            )?);
        }
        items.push(GroundedContinuityItemV2 {
            source_refs,
            text,
            artifact_ref: None,
            receipt_ref: None,
        });
    }
    Ok(items)
}

#[cfg(test)]
#[path = "tests/continuity_v2_tests.rs"]
mod tests;
