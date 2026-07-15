use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::compaction_v2::{
    CompactionAppliedV2, CompactionAttemptState, CompactionAttemptTerminal, CompactionCursor,
    CompactionLifecycleProjection, compaction_lifecycle_event_id, compaction_session_id,
    compaction_started_event_id,
};
use super::*;
use crate::{
    ArtifactId, ContextSensitivity, ContextTrustLevel, EventId, FrozenProviderRequestMaterial,
    MessageRole, PortableCompactionEconomicsV1, RequestFitProof, TaskMemoryId, TaskMemoryV1,
    TokenMeasurementBinding, TokenMeasurementScope, projection_apply_decision,
};

/// Schema version for the V2 compaction sidecar projection.
pub const COMPACTION_SIDECAR_PROJECTION_SCHEMA_VERSION: u16 = 1;
/// Only supported durable TaskMemory sidecar schema in this pre-release build.
pub const TASK_MEMORY_RECORDED_V1_SCHEMA_VERSION: u16 = 1;
/// Only supported continuation-checkpoint binding schema in this pre-release build.
pub const CONTINUATION_CHECKPOINT_V1_SCHEMA_VERSION: u16 = 1;
/// Maximum number of model-owned continuity items accepted in one checkpoint section.
pub const MAX_CONTINUATION_CHECKPOINT_SECTION_ITEMS: usize = 128;
/// A checkpoint must remain a bounded continuation artifact, not a second raw transcript.
pub const MAX_CONTINUATION_CHECKPOINT_ITEM_BYTES: usize = 16 * 1024;

/// How the checkpoint is materialized for the next provider turn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationCheckpointKind {
    None,
    ProviderNative,
    PortableSemantic,
}

/// Durable, non-secret identity and admission proof for the real next provider request.
///
/// The frozen request bytes themselves remain process-local. This record carries only the
/// process-keyed fingerprint, profile binding, and proof needed to audit the activation decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ContinuationTargetRequestFitV1 {
    pub material_fingerprint: String,
    pub binding: TokenMeasurementBinding,
    pub proof: RequestFitProof,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_economics: Option<PortableCompactionEconomicsV1>,
}

impl ContinuationTargetRequestFitV1 {
    fn validate_shape(&self) -> Result<()> {
        self.proof.validate_for(
            &self.material_fingerprint,
            TokenMeasurementScope::RenderedTargetInput,
            &self.binding,
        )?;
        if let Some(economics) = &self.portable_economics {
            economics.validate_for_after(&self.material_fingerprint, &self.proof, &self.binding)?;
        }
        Ok(())
    }

    /// Validates that this durable proof belongs to the exact frozen request about to be sent.
    ///
    /// A persisted fingerprint deliberately does not contain request bytes, so this check must
    /// run at the provider-send boundary with the in-process frozen material. Reloading a session
    /// can validate the durable proof shape, but cannot substitute for this request-bound check.
    ///
    /// # Errors
    ///
    /// Returns an error when the request scope, provider/model identity, or token proof does not
    /// match the frozen request material.
    pub fn validate_for_frozen_request(
        &self,
        expected_session_scope_id: &str,
        frozen_request: &FrozenProviderRequestMaterial,
    ) -> Result<()> {
        if frozen_request.session_scope_id() != expected_session_scope_id {
            bail!("continuation target request material belongs to a different session scope");
        }
        let request = frozen_request.request();
        if request.provider_name != self.binding.provider_name
            || request.model_name != self.binding.model_name
        {
            bail!("continuation target request provider or model does not match token proof");
        }
        self.proof.validate_for(
            frozen_request.fingerprint(),
            TokenMeasurementScope::RenderedTargetInput,
            &self.binding,
        )
    }
}

/// Exact durable transcript reference used by a portable continuation checkpoint.
///
/// IDs are assigned by the deterministic source catalog; model output may select only catalog
/// event IDs and can never synthesize a free-form durable reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ContinuationSourceRef {
    pub session_id: crate::SessionId,
    pub stream_sequence: u64,
    pub event_id: EventId,
    pub message_id: Option<String>,
    pub tool_call_id: Option<String>,
    pub artifact_id: Option<ArtifactId>,
}

impl ContinuationSourceRef {
    fn validate_shape(&self) -> Result<()> {
        if self.session_id.trim().is_empty()
            || self.stream_sequence == 0
            || self.event_id.trim().is_empty()
            || self
                .message_id
                .as_deref()
                .is_some_and(|message_id| message_id.trim().is_empty())
            || self
                .tool_call_id
                .as_deref()
                .is_some_and(|tool_call_id| tool_call_id.trim().is_empty())
            || self
                .artifact_id
                .as_deref()
                .is_some_and(|artifact_id| artifact_id.trim().is_empty())
        {
            bail!("continuation source reference is invalid");
        }
        Ok(())
    }
}

/// Deterministic origin of one checkpoint item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationItemOrigin {
    DurableUser,
    DurableAssistant,
    DurableTool,
    ModelGenerated,
}

/// Authority carried by a checkpoint item. Model-generated items can never become facts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationItemAuthority {
    UserInstruction,
    Observation,
    ModelGeneratedUnverified,
}

/// Evidence status independently rendered alongside item authority.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationEvidenceStatus {
    DurableSource,
    ModelGeneratedUnverified,
}

/// Whether an item remains valid independently of the captured workspace snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationSnapshotScope {
    SnapshotIndependent,
    CapturedAt(crate::WorkspaceSnapshotId),
}

/// How an item was redacted before it became part of a continuation checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationRedaction {
    Unmodified,
    Sanitized {
        policy_id: String,
        source_content_hash: String,
    },
}

/// Priority used only for bounded continuation rendering; it does not grant authority.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ContinuationItemPriority {
    Critical,
    Normal,
}

/// One portable checkpoint item with deterministic metadata and closed-catalog provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ContinuationItemV1 {
    pub text: String,
    pub source_refs: Vec<ContinuationSourceRef>,
    pub origin: ContinuationItemOrigin,
    pub authority: ContinuationItemAuthority,
    pub trust_level: ContextTrustLevel,
    pub sensitivity: ContextSensitivity,
    pub redaction: ContinuationRedaction,
    pub egress_decision_event_id: Option<EventId>,
    pub snapshot_scope: ContinuationSnapshotScope,
    pub evidence_status: ContinuationEvidenceStatus,
    pub priority: ContinuationItemPriority,
}

impl ContinuationItemV1 {
    fn validate_shape(&self) -> Result<()> {
        if self.text.trim().is_empty() || self.text.len() > MAX_CONTINUATION_CHECKPOINT_ITEM_BYTES {
            bail!("continuation checkpoint item text is invalid or exceeds its bounded size");
        }
        if self.source_refs.is_empty() {
            bail!("continuation checkpoint item must have durable source references");
        }
        let mut sources = BTreeSet::new();
        for source in &self.source_refs {
            source.validate_shape()?;
            if !sources.insert(source.clone()) {
                bail!("continuation checkpoint item has duplicate source references");
            }
        }
        if self
            .egress_decision_event_id
            .as_deref()
            .is_some_and(|event_id| event_id.trim().is_empty())
        {
            bail!("continuation checkpoint egress decision event id is empty");
        }
        match &self.redaction {
            ContinuationRedaction::Unmodified => {}
            ContinuationRedaction::Sanitized {
                policy_id,
                source_content_hash,
            } if !policy_id.trim().is_empty() && !source_content_hash.trim().is_empty() => {}
            ContinuationRedaction::Sanitized { .. } => {
                bail!("continuation checkpoint sanitization metadata is invalid");
            }
        }
        if let ContinuationSnapshotScope::CapturedAt(snapshot) = &self.snapshot_scope
            && snapshot.trim().is_empty()
        {
            bail!("continuation checkpoint captured snapshot is empty");
        }
        Ok(())
    }
}

/// Strict JSON body accepted from the semantic compressor. It deliberately contains only
/// model-owned continuity sections; durable task facts and pinned user constraints are rebuilt
/// locally and are not writable by the model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ContinuationModelOutputV1 {
    pub in_progress: Vec<ContinuationModelOutputItemV1>,
    pub pending_actions: Vec<ContinuationModelOutputItemV1>,
    pub provider_continuity: Vec<ContinuationModelOutputItemV1>,
    pub model_notes: Vec<ContinuationModelOutputItemV1>,
}

/// One source-selected model continuation note from the strict compressor JSON response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ContinuationModelOutputItemV1 {
    pub text: String,
    pub source_event_ids: Vec<EventId>,
    pub priority: ContinuationItemPriority,
}

/// Closed deterministic catalog exposed to the semantic compressor for one exact fold plan.
///
/// The source text is intentionally in-memory only. Persisted checkpoints keep just validated
/// references plus the exact user constraints that must survive the fold.
#[derive(Clone)]
pub struct ContinuationSourceCatalog {
    session_id: crate::SessionId,
    entries: BTreeMap<EventId, ContinuationSourceCatalogEntry>,
}

#[derive(Clone)]
struct ContinuationSourceCatalogEntry {
    source: ContinuationSourceRef,
    role: MessageRole,
    text: Option<String>,
    trust_level: ContextTrustLevel,
    sensitivity: ContextSensitivity,
}

impl ContinuationSourceCatalog {
    /// Builds the closed source catalog for one exact safe-fold plan.
    ///
    /// # Errors
    ///
    /// Returns an error if the plan is stale or any folded event fails to decode as a durable
    /// provider-visible message.
    pub fn from_fold_plan(
        records: &[SessionStreamRecord],
        plan: &super::compaction_plan::CompactionFoldPlan,
    ) -> Result<Self> {
        plan.validate_against(records)?;
        let session_id = plan.session_id.clone();
        let external_message_ids = records
            .iter()
            .filter_map(|record| session_entry_from_stored_event(record.stored_event()).transpose())
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(ControlEntry::ExternalProvenance(provenance)) => {
                    Some(provenance.message_id)
                }
                _ => None,
            })
            .collect::<BTreeSet<_>>();

        let by_event_id = records
            .iter()
            .map(|record| (record.event_id(), record.stored_event()))
            .collect::<BTreeMap<_, _>>();
        let mut entries = BTreeMap::new();
        for event_id in &plan.folded_event_ids {
            let event = by_event_id
                .get(event_id.as_str())
                .copied()
                .expect("validated compaction plan references existing folded events");
            let entry = session_entry_from_stored_event(event)?
                .context("folded checkpoint source is not a provider-visible message")?;
            let message = match entry {
                SessionLogEntry::User(message)
                | SessionLogEntry::Assistant(message)
                | SessionLogEntry::ToolResult(message) => message,
                SessionLogEntry::Control(_) => {
                    bail!("folded checkpoint source cannot be a control entry");
                }
            };
            let (trust_level, sensitivity) = if external_message_ids.contains(&message.id) {
                (
                    ContextTrustLevel::ExternalUntrusted,
                    ContextSensitivity::External,
                )
            } else {
                match message.role {
                    MessageRole::User => {
                        (ContextTrustLevel::UserProvided, ContextSensitivity::Public)
                    }
                    MessageRole::Assistant => (
                        ContextTrustLevel::ToolObservation,
                        ContextSensitivity::Public,
                    ),
                    MessageRole::Tool => (
                        ContextTrustLevel::ToolObservation,
                        ContextSensitivity::Repository,
                    ),
                    MessageRole::System => {
                        bail!("folded checkpoint source cannot be a system message")
                    }
                }
            };
            let source = ContinuationSourceRef {
                session_id: session_id.clone(),
                stream_sequence: event.stream_sequence,
                event_id: event.event_id.clone(),
                message_id: Some(message.id.clone()),
                tool_call_id: message.tool_call_id.clone(),
                artifact_id: None,
            };
            source.validate_shape()?;
            if entries
                .insert(
                    event.event_id.clone(),
                    ContinuationSourceCatalogEntry {
                        source,
                        role: message.role,
                        text: message.content,
                        trust_level,
                        sensitivity,
                    },
                )
                .is_some()
            {
                bail!("continuation source catalog contains duplicate folded event ids");
            }
        }
        Ok(Self {
            session_id,
            entries,
        })
    }

    fn entry(&self, event_id: &str) -> Option<&ContinuationSourceCatalogEntry> {
        self.entries.get(event_id)
    }

    fn pinned_user_items(&self) -> Result<Vec<ContinuationItemV1>> {
        let items = self
            .entries
            .values()
            .filter(|entry| entry.role == MessageRole::User)
            .filter_map(|entry| {
                entry
                    .text
                    .as_ref()
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| ContinuationItemV1 {
                        text: text.clone(),
                        source_refs: vec![entry.source.clone()],
                        origin: ContinuationItemOrigin::DurableUser,
                        authority: ContinuationItemAuthority::UserInstruction,
                        trust_level: entry.trust_level,
                        sensitivity: entry.sensitivity,
                        redaction: ContinuationRedaction::Unmodified,
                        egress_decision_event_id: None,
                        snapshot_scope: ContinuationSnapshotScope::SnapshotIndependent,
                        evidence_status: ContinuationEvidenceStatus::DurableSource,
                        priority: ContinuationItemPriority::Critical,
                    })
            })
            .collect::<Vec<_>>();
        for item in &items {
            item.validate_shape()?;
        }
        Ok(items)
    }
}

/// Provider-visible continuation checkpoint derived from one TaskMemory sidecar and a closed
/// durable source catalog. This is a continuation view, never a second durable fact store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ContinuationCheckpointV1 {
    pub schema_version: u16,
    pub kind: ContinuationCheckpointKind,
    pub language: String,
    pub task_memory_id: Option<TaskMemoryId>,
    pub valid_for_snapshot: Option<crate::WorkspaceSnapshotId>,
    pub source_plan_cursor: Option<ProjectionCursor>,
    pub requested_tail_message_count: Option<usize>,
    pub prior_folded_through: Option<CompactionCursor>,
    pub target_request_fit: Option<ContinuationTargetRequestFitV1>,
    pub pinned_user_constraints: Vec<ContinuationItemV1>,
    pub in_progress: Vec<ContinuationItemV1>,
    pub pending_actions: Vec<ContinuationItemV1>,
    pub provider_continuity: Vec<ContinuationItemV1>,
    pub model_notes: Vec<ContinuationItemV1>,
}

impl ContinuationCheckpointV1 {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            schema_version: CONTINUATION_CHECKPOINT_V1_SCHEMA_VERSION,
            kind: ContinuationCheckpointKind::None,
            language: "en".to_owned(),
            task_memory_id: None,
            valid_for_snapshot: None,
            source_plan_cursor: None,
            requested_tail_message_count: None,
            prior_folded_through: None,
            target_request_fit: None,
            pinned_user_constraints: Vec::new(),
            in_progress: Vec::new(),
            pending_actions: Vec::new(),
            provider_continuity: Vec::new(),
            model_notes: Vec::new(),
        }
    }

    /// Creates a typed checkpoint binding for a non-portable continuation candidate.
    ///
    /// Portable semantic compaction must use [`Self::from_catalog_and_model_output`] so its
    /// pinned constraints and model-owned sections are validated. This constructor exists for
    /// provider-native candidates that have their own validated continuation payload.
    #[must_use]
    pub fn bound_to(
        task_memory_id: impl Into<TaskMemoryId>,
        valid_for_snapshot: impl Into<crate::WorkspaceSnapshotId>,
    ) -> Self {
        Self {
            kind: ContinuationCheckpointKind::ProviderNative,
            task_memory_id: Some(task_memory_id.into()),
            valid_for_snapshot: Some(valid_for_snapshot.into()),
            ..Self::empty()
        }
    }

    /// Builds a portable checkpoint from deterministic TaskMemory/pinned-user sources and one
    /// strict model output. The model cannot write durable facts or source metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the language, source references, model output, or durable bindings
    /// are invalid.
    pub fn from_catalog_and_model_output(
        language: impl Into<String>,
        task_memory: &TaskMemoryV1,
        catalog: &ContinuationSourceCatalog,
        plan: &CompactionFoldPlan,
        output: ContinuationModelOutputV1,
    ) -> Result<Self> {
        task_memory.validate()?;
        if plan.session_id != catalog.session_id {
            bail!("continuation checkpoint plan and source catalog sessions do not match");
        }
        let valid_for_snapshot = task_memory.valid_for_snapshot.clone();
        let checkpoint = Self {
            schema_version: CONTINUATION_CHECKPOINT_V1_SCHEMA_VERSION,
            kind: ContinuationCheckpointKind::PortableSemantic,
            language: language.into(),
            task_memory_id: Some(task_memory.memory_id.clone()),
            valid_for_snapshot: Some(valid_for_snapshot.clone()),
            source_plan_cursor: Some(plan.base_stream_cursor.clone()),
            requested_tail_message_count: Some(plan.requested_tail_message_count),
            prior_folded_through: plan.prior_folded_through.clone(),
            target_request_fit: None,
            pinned_user_constraints: catalog.pinned_user_items()?,
            in_progress: model_items_from_output(catalog, &valid_for_snapshot, output.in_progress)?,
            pending_actions: model_items_from_output(
                catalog,
                &valid_for_snapshot,
                output.pending_actions,
            )?,
            provider_continuity: model_items_from_output(
                catalog,
                &valid_for_snapshot,
                output.provider_continuity,
            )?,
            model_notes: model_items_from_output(catalog, &valid_for_snapshot, output.model_notes)?,
        };
        checkpoint.validate_against_catalog(task_memory, catalog)?;
        Ok(checkpoint)
    }

    /// Deterministically renders the fixed portable continuation sections for a provider request.
    /// Model-owned notes are visibly labelled unverified; durable TaskMemory remains the source of
    /// objective, decisions, file, command, verification, failure, risk, and issue facts.
    ///
    /// # Errors
    ///
    /// Returns an error when bindings are inconsistent or the checkpoint cannot be rendered
    /// within its bounded item contract.
    pub fn render_for_provider(&self, task_memory: &TaskMemoryV1) -> Result<crate::ModelMessage> {
        self.validate_task_memory_binding(task_memory)?;
        if self.kind != ContinuationCheckpointKind::PortableSemantic {
            bail!("only portable continuation checkpoints render as provider messages");
        }
        let content = render_portable_checkpoint(self, task_memory)?;
        let id = format!(
            "continuation-checkpoint:{}",
            crate::event::canonical_json_content_hash(&serde_json::json!({
                "schema_version": self.schema_version,
                "kind": self.kind,
                "language": self.language,
                "task_memory_id": self.task_memory_id,
                "valid_for_snapshot": self.valid_for_snapshot,
                "pinned_user_constraints": self.pinned_user_constraints,
                "in_progress": self.in_progress,
                "pending_actions": self.pending_actions,
                "provider_continuity": self.provider_continuity,
                "model_notes": self.model_notes,
            }))?
        );
        Ok(crate::ModelMessage {
            id,
            role: MessageRole::Assistant,
            content: Some(content),
            tool_calls: Vec::new(),
            tool_call_id: None,
            assistant_kind: None,
            image_attachments: Vec::new(),
        })
    }

    /// Verifies that a frozen provider request contains this exact portable checkpoint once and
    /// that its persisted token proof is bound to the same request.
    ///
    /// Call this immediately before provider dispatch. It is intentionally separate from durable
    /// replay validation because the frozen request bytes are process-local and never persisted.
    ///
    /// # Errors
    ///
    /// Returns an error when this is not a portable checkpoint, the checkpoint/task-memory
    /// binding is invalid, the frozen request has a different scope/proof, or it does not contain
    /// exactly one copy of the rendered checkpoint message.
    pub fn validate_for_frozen_target_request(
        &self,
        task_memory: &TaskMemoryV1,
        expected_session_scope_id: &str,
        frozen_request: &FrozenProviderRequestMaterial,
    ) -> Result<()> {
        self.validate_task_memory_binding(task_memory)?;
        if self.kind != ContinuationCheckpointKind::PortableSemantic {
            bail!("only portable continuation checkpoints validate target requests");
        }
        let target_request_fit = self
            .target_request_fit
            .as_ref()
            .context("portable continuation checkpoint has no target request-fit proof")?;
        target_request_fit
            .validate_for_frozen_request(expected_session_scope_id, frozen_request)?;
        let rendered = self.render_for_provider(task_memory)?;
        let rendered_value = serde_json::to_value(&rendered)
            .context("failed to encode rendered continuation checkpoint")?;
        let occurrences = frozen_request
            .request()
            .messages
            .iter()
            .map(serde_json::to_value)
            .collect::<std::result::Result<Vec<_>, serde_json::Error>>()?
            .iter()
            .filter(|message| *message == &rendered_value)
            .count();
        if occurrences != 1 {
            bail!(
                "frozen target request must contain the rendered continuation checkpoint exactly once"
            );
        }
        Ok(())
    }

    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.schema_version != CONTINUATION_CHECKPOINT_V1_SCHEMA_VERSION {
            bail!("unsupported continuation checkpoint schema version");
        }
        if self.language.trim().is_empty() || self.language.len() > 32 {
            bail!("continuation checkpoint language is invalid");
        }
        if self
            .task_memory_id
            .as_deref()
            .is_some_and(|memory_id| memory_id.trim().is_empty())
        {
            bail!("continuation checkpoint task memory id is empty");
        }
        if self
            .valid_for_snapshot
            .as_deref()
            .is_some_and(|snapshot| snapshot.trim().is_empty())
        {
            bail!("continuation checkpoint snapshot id is empty");
        }
        if self.task_memory_id.is_some() != self.valid_for_snapshot.is_some() {
            bail!("continuation checkpoint memory and snapshot bindings must appear together");
        }
        match self.kind {
            ContinuationCheckpointKind::None => {
                if self.task_memory_id.is_some()
                    || self.source_plan_cursor.is_some()
                    || self.requested_tail_message_count.is_some()
                    || self.prior_folded_through.is_some()
                    || self.target_request_fit.is_some()
                {
                    bail!("empty continuation checkpoint has durable bindings");
                }
            }
            ContinuationCheckpointKind::ProviderNative => {
                if self.task_memory_id.is_none()
                    || self.source_plan_cursor.is_some()
                    || self.requested_tail_message_count.is_some()
                    || self.prior_folded_through.is_some()
                    || self.target_request_fit.is_some()
                {
                    bail!("provider-native continuation checkpoint bindings are invalid");
                }
            }
            ContinuationCheckpointKind::PortableSemantic => {
                let cursor = self
                    .source_plan_cursor
                    .as_ref()
                    .context("portable continuation checkpoint has no source plan cursor")?;
                if self.task_memory_id.is_none()
                    || self
                        .requested_tail_message_count
                        .is_none_or(|count| count == 0)
                    || cursor.projection_schema_version != COMPACTION_FOLD_PLAN_SCHEMA_VERSION
                    || cursor.session_id.trim().is_empty()
                    || cursor.last_applied_stream_sequence == 0
                    || cursor.last_applied_event_id.trim().is_empty()
                    || cursor.last_applied_record_checksum.trim().is_empty()
                {
                    bail!("portable continuation checkpoint bindings are invalid");
                }
            }
        }
        if let Some(target_request_fit) = &self.target_request_fit {
            target_request_fit.validate_shape()?;
        }
        for section in [
            &self.pinned_user_constraints,
            &self.in_progress,
            &self.pending_actions,
            &self.provider_continuity,
            &self.model_notes,
        ] {
            if section.len() > MAX_CONTINUATION_CHECKPOINT_SECTION_ITEMS {
                bail!("continuation checkpoint section exceeds its item limit");
            }
            for item in section {
                item.validate_shape()?;
            }
        }
        if self.kind == ContinuationCheckpointKind::None
            && [
                &self.pinned_user_constraints,
                &self.in_progress,
                &self.pending_actions,
                &self.provider_continuity,
                &self.model_notes,
            ]
            .into_iter()
            .any(|section| !section.is_empty())
        {
            bail!("empty continuation checkpoint cannot contain continuation items");
        }
        Ok(())
    }

    fn validate_against_catalog(
        &self,
        task_memory: &TaskMemoryV1,
        catalog: &ContinuationSourceCatalog,
    ) -> Result<()> {
        self.validate_task_memory_binding(task_memory)?;
        if self.kind != ContinuationCheckpointKind::PortableSemantic {
            bail!("portable source-catalog validation requires a portable checkpoint");
        }
        if catalog.session_id.trim().is_empty() {
            bail!("continuation source catalog session id is empty");
        }
        let expected_pinned = catalog.pinned_user_items()?;
        if self.pinned_user_constraints != expected_pinned {
            bail!("continuation checkpoint does not preserve exact pinned user constraints");
        }
        for section in [
            &self.in_progress,
            &self.pending_actions,
            &self.provider_continuity,
            &self.model_notes,
        ] {
            for item in section {
                validate_model_item_against_catalog(
                    item,
                    catalog,
                    &task_memory.valid_for_snapshot,
                )?;
            }
        }
        Ok(())
    }

    fn validate_for_activation(
        &self,
        task_memory: &TaskMemoryV1,
        catalog: &ContinuationSourceCatalog,
    ) -> Result<()> {
        self.validate_against_catalog(task_memory, catalog)?;
        self.target_request_fit
            .as_ref()
            .context("portable continuation checkpoint has no target request-fit proof")?
            .validate_shape()?;
        self.target_request_fit
            .as_ref()
            .and_then(|fit| fit.portable_economics.as_ref())
            .context("portable continuation checkpoint has no before/after economics proof")?
            .validate_for_after(
                self.target_request_fit
                    .as_ref()
                    .expect("target proof was checked above")
                    .material_fingerprint
                    .as_str(),
                &self
                    .target_request_fit
                    .as_ref()
                    .expect("target proof was checked above")
                    .proof,
                &self
                    .target_request_fit
                    .as_ref()
                    .expect("target proof was checked above")
                    .binding,
            )
    }

    pub(crate) fn attach_target_request_fit(
        &mut self,
        target_request_fit: ContinuationTargetRequestFitV1,
    ) -> Result<()> {
        if self.kind != ContinuationCheckpointKind::PortableSemantic {
            bail!("only portable continuation checkpoints carry target request-fit proof");
        }
        target_request_fit.validate_shape()?;
        self.target_request_fit = Some(target_request_fit);
        Ok(())
    }

    fn validate_task_memory_binding(&self, task_memory: &TaskMemoryV1) -> Result<()> {
        self.validate_shape()?;
        task_memory.validate()?;
        if self.task_memory_id.as_deref() != Some(task_memory.memory_id.as_str())
            || self.valid_for_snapshot.as_deref() != Some(task_memory.valid_for_snapshot.as_str())
        {
            bail!("continuation checkpoint does not match task memory binding");
        }
        Ok(())
    }
}

fn model_items_from_output(
    catalog: &ContinuationSourceCatalog,
    valid_for_snapshot: &str,
    output: Vec<ContinuationModelOutputItemV1>,
) -> Result<Vec<ContinuationItemV1>> {
    if output.len() > MAX_CONTINUATION_CHECKPOINT_SECTION_ITEMS {
        bail!("continuation model output section exceeds its item limit");
    }
    output
        .into_iter()
        .map(|item| {
            if item.text.trim().is_empty()
                || item.text.len() > MAX_CONTINUATION_CHECKPOINT_ITEM_BYTES
                || contains_authority_claim(&item.text)
            {
                bail!("continuation model output contains an invalid authority claim");
            }
            let mut source_refs = Vec::with_capacity(item.source_event_ids.len());
            let mut trust_level = ContextTrustLevel::UserProvided;
            let mut sensitivity = ContextSensitivity::Public;
            for event_id in item.source_event_ids {
                let source = catalog.entry(&event_id).with_context(|| {
                    format!("continuation model output references unknown source {event_id}")
                })?;
                source_refs.push(source.source.clone());
                trust_level = stricter_trust_level(trust_level, source.trust_level);
                sensitivity = stricter_sensitivity(sensitivity, source.sensitivity);
            }
            let item = ContinuationItemV1 {
                text: item.text,
                source_refs,
                origin: ContinuationItemOrigin::ModelGenerated,
                authority: ContinuationItemAuthority::ModelGeneratedUnverified,
                trust_level,
                sensitivity,
                redaction: ContinuationRedaction::Unmodified,
                egress_decision_event_id: None,
                snapshot_scope: ContinuationSnapshotScope::CapturedAt(
                    valid_for_snapshot.to_owned(),
                ),
                evidence_status: ContinuationEvidenceStatus::ModelGeneratedUnverified,
                priority: item.priority,
            };
            item.validate_shape()?;
            Ok(item)
        })
        .collect()
}

fn validate_model_item_against_catalog(
    item: &ContinuationItemV1,
    catalog: &ContinuationSourceCatalog,
    valid_for_snapshot: &str,
) -> Result<()> {
    if item.origin != ContinuationItemOrigin::ModelGenerated
        || item.authority != ContinuationItemAuthority::ModelGeneratedUnverified
        || item.evidence_status != ContinuationEvidenceStatus::ModelGeneratedUnverified
        || item.egress_decision_event_id.is_some()
        || item.redaction != ContinuationRedaction::Unmodified
        || item.snapshot_scope
            != ContinuationSnapshotScope::CapturedAt(valid_for_snapshot.to_owned())
        || contains_authority_claim(&item.text)
    {
        bail!("continuation model item violates the unverified authority contract");
    }
    let mut expected_trust = ContextTrustLevel::UserProvided;
    let mut expected_sensitivity = ContextSensitivity::Public;
    for source in &item.source_refs {
        let catalog_entry = catalog.entry(&source.event_id).with_context(|| {
            format!(
                "continuation item references unknown source {}",
                source.event_id
            )
        })?;
        if source != &catalog_entry.source {
            bail!("continuation item source reference does not match the closed catalog");
        }
        expected_trust = stricter_trust_level(expected_trust, catalog_entry.trust_level);
        expected_sensitivity =
            stricter_sensitivity(expected_sensitivity, catalog_entry.sensitivity);
    }
    if item.trust_level != expected_trust || item.sensitivity != expected_sensitivity {
        bail!("continuation item metadata does not match its durable sources");
    }
    Ok(())
}

fn contains_authority_claim(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    [
        "completed",
        "verified",
        "approved",
        "已经完成",
        "已完成",
        "已验证",
        "已批准",
    ]
    .iter()
    .any(|claim| normalized.contains(claim))
}

fn stricter_trust_level(
    current: ContextTrustLevel,
    candidate: ContextTrustLevel,
) -> ContextTrustLevel {
    use ContextTrustLevel::*;
    match (current, candidate) {
        (ExternalUntrusted, _) | (_, ExternalUntrusted) => ExternalUntrusted,
        (ExtensionProvided, _) | (_, ExtensionProvided) => ExtensionProvided,
        (ToolObservation, _) | (_, ToolObservation) => ToolObservation,
        (UntrustedRepositoryData, _) | (_, UntrustedRepositoryData) => UntrustedRepositoryData,
        (WorkspaceInstruction, _) | (_, WorkspaceInstruction) => WorkspaceInstruction,
        (System, _) | (_, System) => System,
        _ => UserProvided,
    }
}

fn stricter_sensitivity(
    current: ContextSensitivity,
    candidate: ContextSensitivity,
) -> ContextSensitivity {
    use ContextSensitivity::*;
    match (current, candidate) {
        (Secret, _) | (_, Secret) => Secret,
        (PotentialSecret, _) | (_, PotentialSecret) => PotentialSecret,
        (External, _) | (_, External) => External,
        (Repository, _) | (_, Repository) => Repository,
        _ => Public,
    }
}

fn render_portable_checkpoint(
    checkpoint: &ContinuationCheckpointV1,
    memory: &TaskMemoryV1,
) -> Result<String> {
    let mut rendered = String::from(
        "Sigil continuation checkpoint (durable facts and unverified model notes):\n\n",
    );
    render_section(
        &mut rendered,
        "Goal",
        std::iter::once(memory.objective.as_str()),
    );
    if let Some(plan) = &memory.active_plan {
        render_section(
            &mut rendered,
            "Active Plan",
            plan.steps
                .iter()
                .map(|step| format!("[{:?}] {}", step.status, step.title)),
        );
    }
    render_section(
        &mut rendered,
        "Constraints & Preferences",
        checkpoint
            .pinned_user_constraints
            .iter()
            .map(render_pinned_constraint),
    );
    render_section(
        &mut rendered,
        "Progress — Done",
        memory
            .files_changed
            .iter()
            .map(|file| format!("changed file: {}", file.path.display()))
            .chain(
                memory
                    .commands_run
                    .iter()
                    .map(|command| format!("command receipt: {command}")),
            )
            .chain(
                memory
                    .verification_results
                    .iter()
                    .map(|receipt| format!("verification receipt: {receipt}")),
            ),
    );
    render_section(
        &mut rendered,
        "Progress — In Progress",
        checkpoint.in_progress.iter().map(render_unverified_item),
    );
    render_section(
        &mut rendered,
        "Progress — Blocked",
        memory
            .failed_attempts
            .iter()
            .map(|attempt| {
                format!(
                    "failed attempt {}{}",
                    attempt.attempt_id,
                    attempt
                        .summary
                        .as_deref()
                        .map(|summary| format!(": {summary}"))
                        .unwrap_or_default()
                )
            })
            .chain(
                memory
                    .unresolved_issues
                    .iter()
                    .map(|issue| issue.text.clone()),
            ),
    );
    render_section(
        &mut rendered,
        "Key Decisions",
        memory
            .decisions
            .iter()
            .map(|decision| decision.decision.text.clone()),
    );
    render_section(
        &mut rendered,
        "Next Steps",
        checkpoint
            .pending_actions
            .iter()
            .map(render_unverified_item),
    );
    render_section(
        &mut rendered,
        "Critical Context",
        memory
            .risks
            .iter()
            .map(|risk| risk.text.clone())
            .chain(checkpoint.model_notes.iter().map(render_unverified_item))
            .chain(
                checkpoint
                    .provider_continuity
                    .iter()
                    .map(render_unverified_item),
            ),
    );
    render_section(
        &mut rendered,
        "Relevant Files",
        memory
            .files_changed
            .iter()
            .map(|file| file.path.display().to_string()),
    );
    render_section(
        &mut rendered,
        "Validation / Tool Results",
        memory
            .commands_run
            .iter()
            .map(|command| format!("command receipt: {command}"))
            .chain(
                memory
                    .verification_results
                    .iter()
                    .map(|receipt| format!("verification receipt: {receipt}")),
            ),
    );
    Ok(rendered)
}

fn render_section<I, S>(rendered: &mut String, heading: &str, items: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    rendered.push_str("## ");
    rendered.push_str(heading);
    rendered.push('\n');
    let mut any = false;
    for item in items {
        any = true;
        rendered.push_str("- ");
        rendered.push_str(item.as_ref());
        rendered.push('\n');
    }
    if !any {
        rendered.push_str("- none recorded\n");
    }
    rendered.push('\n');
}

fn render_pinned_constraint(item: &ContinuationItemV1) -> String {
    let source = item
        .source_refs
        .first()
        .expect("validated pinned user constraint has one source");
    format!("{} [durable user event: {}]", item.text, source.event_id)
}

fn render_unverified_item(item: &ContinuationItemV1) -> String {
    format!("[model-generated, unverified] {}", item.text)
}

/// Inactive durable TaskMemory payload, activated only by a matching AppliedV2 terminal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskMemoryRecordedV1 {
    pub schema_version: u16,
    pub derived_through: CompactionCursor,
    pub memory: TaskMemoryV1,
    pub content_hash: String,
    pub byte_size: u64,
}

impl TaskMemoryRecordedV1 {
    /// Builds a self-verifying current-schema sidecar record.
    pub fn new(derived_through: CompactionCursor, memory: TaskMemoryV1) -> Result<Self> {
        memory.validate()?;
        let (content_hash, byte_size) = task_memory_identity(&memory)?;
        Ok(Self {
            schema_version: TASK_MEMORY_RECORDED_V1_SCHEMA_VERSION,
            derived_through,
            memory,
            content_hash,
            byte_size,
        })
    }

    pub(crate) fn validate_shape(&self, session_id: &str, record_sequence: u64) -> Result<()> {
        if self.schema_version != TASK_MEMORY_RECORDED_V1_SCHEMA_VERSION {
            bail!("unsupported task memory record schema version");
        }
        self.derived_through
            .validate_for_session(session_id, record_sequence)?;
        self.memory.validate()?;
        let (content_hash, byte_size) = task_memory_identity(&self.memory)?;
        if self.content_hash != content_hash {
            bail!("task memory record content hash does not match memory payload");
        }
        if self.byte_size != byte_size {
            bail!("task memory record byte size does not match memory payload");
        }
        Ok(())
    }
}

/// The reason a previously activated TaskMemory sidecar can no longer be resolved.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskMemoryInvalidationReason {
    Explicit,
    BranchLineageChanged,
    Corrupted,
}

/// Durable invalidation of one activated TaskMemory sidecar.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct TaskMemoryInvalidatedEntry {
    pub task_memory_id: TaskMemoryId,
    pub reason: TaskMemoryInvalidationReason,
    pub invalidated_by_event_id: EventId,
}

impl TaskMemoryInvalidatedEntry {
    pub(crate) fn validate_shape(&self) -> Result<()> {
        if self.task_memory_id.trim().is_empty() {
            bail!("task memory invalidation id is empty");
        }
        if self.invalidated_by_event_id.trim().is_empty() {
            bail!("task memory invalidation source event id is empty");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedTaskMemory {
    event_id: EventId,
    stream_sequence: u64,
    started_event_id: EventId,
    attempt_id: String,
    entry: TaskMemoryRecordedV1,
}

/// An activated sidecar resolved from one AppliedV2 terminal, without any context injection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCompactionSidecar {
    pub compaction_id: String,
    pub applied_event_id: EventId,
    pub applied_stream_sequence: u64,
    pub folded_through: CompactionCursor,
    pub task_memory_event_id: EventId,
    pub task_memory: TaskMemoryV1,
    pub checkpoint: ContinuationCheckpointV1,
}

/// Read-only reconstruction of V2 TaskMemory/checkpoint sidecars.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionSidecarProjection {
    cursor: Option<ProjectionCursor>,
    recorded: BTreeMap<TaskMemoryId, RecordedTaskMemory>,
    invalidated: BTreeSet<TaskMemoryId>,
    active: BTreeMap<String, ResolvedCompactionSidecar>,
}

impl CompactionSidecarProjection {
    /// Rebuilds sidecars without changing the stream or injecting any model-visible context.
    pub fn from_records(records: &[SessionStreamRecord]) -> Result<Self> {
        let lifecycle = CompactionLifecycleProjection::from_records(records)?;
        let mut projection = Self::default();
        let mut invalidations = Vec::new();

        for record in records {
            let event = record.stored_event();
            let decision = projection_apply_decision(projection.cursor.as_ref(), event)?;
            if decision == ProjectionApplyDecision::IgnoreAlreadyApplied {
                continue;
            }
            match decode_stored_event(event.clone())? {
                StoredEventDecode::Known(_) | StoredEventDecode::UnknownNonCritical(_) => {}
            }
            match event.event_kind() {
                Some(DurableEventType::TaskMemoryRecordedV1) => {
                    let entry: TaskMemoryRecordedV1 = decode_sidecar_payload(event)?;
                    entry.validate_shape(&event.session_id, event.stream_sequence)?;
                    let attempt = lifecycle
                        .attempt_for_started_event_id(
                            event.correlation_id.as_deref().unwrap_or_default(),
                        )
                        .context(
                            "task memory sidecar correlation does not reference a compaction start",
                        )?;
                    validate_sidecar_lineage(event, attempt)?;
                    if event.stream_sequence <= attempt.started_stream_sequence {
                        bail!("task memory sidecar must follow its compaction start");
                    }
                    if let Some(terminal) = &attempt.terminal {
                        let terminal_sequence = match terminal {
                            CompactionAttemptTerminal::Applied {
                                stream_sequence, ..
                            }
                            | CompactionAttemptTerminal::Failed {
                                stream_sequence, ..
                            } => *stream_sequence,
                        };
                        if event.stream_sequence >= terminal_sequence {
                            bail!("task memory sidecar must precede its compaction terminal");
                        }
                    }
                    if projection.recorded.contains_key(&entry.memory.memory_id) {
                        bail!(
                            "task memory id {} was recorded more than once",
                            entry.memory.memory_id
                        );
                    }
                    projection.recorded.insert(
                        entry.memory.memory_id.clone(),
                        RecordedTaskMemory {
                            event_id: event.event_id.clone(),
                            stream_sequence: event.stream_sequence,
                            started_event_id: attempt.started_event_id.clone(),
                            attempt_id: attempt.entry.attempt_id.clone(),
                            entry,
                        },
                    );
                }
                Some(DurableEventType::TaskMemoryInvalidated) => {
                    let entry: TaskMemoryInvalidatedEntry = decode_sidecar_payload(event)?;
                    entry.validate_shape()?;
                    invalidations.push((event.clone(), entry));
                }
                Some(_) | None => {}
            }
            projection.cursor =
                Some(record.projection_cursor(COMPACTION_SIDECAR_PROJECTION_SCHEMA_VERSION));
        }

        let mut actions = lifecycle
            .attempts()
            .filter_map(|attempt| match &attempt.terminal {
                Some(CompactionAttemptTerminal::Applied {
                    event_id,
                    stream_sequence,
                    entry,
                }) => Some(SidecarAction::Applied {
                    attempt_id: attempt.entry.attempt_id.clone(),
                    event_id: event_id.clone(),
                    stream_sequence: *stream_sequence,
                    entry: entry.clone(),
                }),
                Some(CompactionAttemptTerminal::Failed { .. }) | None => None,
            })
            .collect::<Vec<_>>();
        for (event, entry) in invalidations {
            actions.push(SidecarAction::Invalidated {
                event: Box::new(event),
                entry,
            });
        }
        actions.sort_by_key(SidecarAction::stream_sequence);
        for action in actions {
            match action {
                SidecarAction::Applied {
                    attempt_id,
                    event_id,
                    stream_sequence,
                    entry,
                } => {
                    let attempt = lifecycle
                        .attempt(&attempt_id)
                        .expect("applied action derives from an existing attempt");
                    projection.activate_applied(
                        records,
                        attempt,
                        &event_id,
                        stream_sequence,
                        &entry,
                    )?;
                }
                SidecarAction::Invalidated { event, entry } => {
                    projection.apply_invalidation(&event, entry)?;
                }
            }
        }
        Ok(projection)
    }

    #[must_use]
    pub fn cursor(&self) -> Option<&ProjectionCursor> {
        self.cursor.as_ref()
    }

    /// Resolves an uninvalidated sidecar for one exact branch, newest AppliedV2 first.
    #[must_use]
    pub fn latest_for_branch(&self, branch_id: Option<&str>) -> Option<&ResolvedCompactionSidecar> {
        self.active
            .values()
            .filter(|sidecar| sidecar.task_memory.branch_id.as_deref() == branch_id)
            .max_by_key(|sidecar| sidecar.applied_stream_sequence)
    }

    #[must_use]
    pub fn resolved_compaction(&self, compaction_id: &str) -> Option<&ResolvedCompactionSidecar> {
        self.active.get(compaction_id)
    }

    #[must_use]
    fn recorded_memory(&self, memory_id: &str) -> Option<&RecordedTaskMemory> {
        self.recorded.get(memory_id)
    }

    #[must_use]
    fn is_invalidated(&self, memory_id: &str) -> bool {
        self.invalidated.contains(memory_id)
    }

    fn active_memory(&self, memory_id: &str) -> Option<&ResolvedCompactionSidecar> {
        self.active
            .values()
            .find(|sidecar| sidecar.task_memory.memory_id == memory_id)
    }

    fn activate_applied(
        &mut self,
        records: &[SessionStreamRecord],
        attempt: &CompactionAttemptState,
        applied_event_id: &str,
        applied_sequence: u64,
        entry: &CompactionAppliedV2,
    ) -> Result<()> {
        if entry.task_memory_id.is_none() {
            ensure_empty_checkpoint(entry)?;
            return Ok(());
        }
        let memory_id = entry.task_memory_id.as_deref().expect("checked above");
        let recorded = self.recorded.get(memory_id).with_context(|| {
            format!("applied compaction references missing task memory {memory_id}")
        })?;
        validate_memory_activation(attempt, applied_sequence, entry, recorded)?;
        validate_portable_checkpoint_activation(records, entry, recorded)?;
        if self
            .active
            .values()
            .any(|sidecar| sidecar.task_memory.memory_id == memory_id)
        {
            bail!("task memory {memory_id} was activated more than once");
        }
        if let Some(supersedes) = &recorded.entry.memory.supersedes {
            let parent_id = entry
                .parent_compaction_id
                .as_deref()
                .context("superseding task memory requires a parent compaction")?;
            let parent = self
                .active
                .get(parent_id)
                .context("superseding task memory parent has no active sidecar")?;
            if &parent.task_memory.memory_id != supersedes {
                bail!("task memory supersedes id does not match parent compaction sidecar");
            }
            if parent.task_memory.branch_id != recorded.entry.memory.branch_id {
                bail!("task memory supersedes lineage crosses branches");
            }
        }
        self.active.insert(
            entry.compaction_id.clone(),
            ResolvedCompactionSidecar {
                compaction_id: entry.compaction_id.clone(),
                applied_event_id: applied_event_id.to_owned(),
                applied_stream_sequence: applied_sequence,
                folded_through: entry.folded_through.clone(),
                task_memory_event_id: recorded.event_id.clone(),
                task_memory: recorded.entry.memory.clone(),
                checkpoint: entry.checkpoint.clone(),
            },
        );
        Ok(())
    }

    fn apply_invalidation(
        &mut self,
        event: &StoredEvent,
        entry: TaskMemoryInvalidatedEntry,
    ) -> Result<()> {
        let recorded = self.recorded.get(&entry.task_memory_id).with_context(|| {
            format!(
                "task memory invalidation references unknown memory {}",
                entry.task_memory_id
            )
        })?;
        let active = self
            .active
            .values()
            .find(|sidecar| sidecar.task_memory.memory_id == entry.task_memory_id)
            .context("task memory invalidation references an inactive memory")?;
        if entry.invalidated_by_event_id != active.applied_event_id {
            bail!("task memory invalidation source does not match its AppliedV2 event");
        }
        if event.stream_sequence <= active.applied_stream_sequence {
            bail!("task memory invalidation must follow its AppliedV2 event");
        }
        if event.correlation_id.as_deref() != Some(recorded.started_event_id.as_str())
            || event.causation_id.as_deref() != Some(recorded.started_event_id.as_str())
        {
            bail!("task memory invalidation must remain in its compaction start lineage");
        }
        if !self.invalidated.insert(entry.task_memory_id.clone()) {
            bail!(
                "task memory {} was invalidated more than once",
                entry.task_memory_id
            );
        }
        self.active
            .retain(|_, sidecar| sidecar.task_memory.memory_id != entry.task_memory_id);
        Ok(())
    }
}

#[derive(Debug)]
enum SidecarAction {
    Applied {
        attempt_id: String,
        event_id: EventId,
        stream_sequence: u64,
        entry: Box<CompactionAppliedV2>,
    },
    Invalidated {
        event: Box<StoredEvent>,
        entry: TaskMemoryInvalidatedEntry,
    },
}

impl SidecarAction {
    fn stream_sequence(&self) -> u64 {
        match self {
            Self::Applied {
                stream_sequence, ..
            } => *stream_sequence,
            Self::Invalidated { event, .. } => event.stream_sequence,
        }
    }
}

pub(super) fn validate_pending_applied_sidecar(
    records: &[SessionStreamRecord],
    entry: &CompactionAppliedV2,
) -> Result<()> {
    let lifecycle = CompactionLifecycleProjection::from_records(records)?;
    let attempt = lifecycle
        .attempt(&entry.attempt_id)
        .context("pending applied compaction attempt is missing")?;
    let projection = CompactionSidecarProjection::from_records(records)?;
    if entry.task_memory_id.is_none() {
        return ensure_empty_checkpoint(entry);
    }
    let memory_id = entry.task_memory_id.as_deref().expect("checked above");
    let recorded = projection.recorded_memory(memory_id).with_context(|| {
        format!("pending applied compaction references missing task memory {memory_id}")
    })?;
    if projection.is_invalidated(memory_id) {
        bail!("pending applied compaction references invalidated task memory {memory_id}");
    }
    validate_memory_activation(attempt, next_stream_sequence(records), entry, recorded)?;
    validate_portable_checkpoint_activation(records, entry, recorded)
}

impl JsonlSessionStore {
    /// Appends an inactive TaskMemory sidecar for one still-open initiated compaction attempt.
    ///
    /// The payload becomes resolver-visible only after a matching `CompactionAppliedV2` terminal.
    pub fn append_task_memory_recorded_v1(
        &self,
        attempt_id: &str,
        entry: TaskMemoryRecordedV1,
    ) -> Result<StoredEvent> {
        if attempt_id.trim().is_empty() {
            bail!("task memory sidecar attempt id is empty");
        }
        let session_id = compaction_session_id(self)?;
        let started_event_id = compaction_started_event_id(self, attempt_id)?;
        let event_id = compaction_lifecycle_event_id(
            &session_id,
            attempt_id,
            &format!("task-memory:{}", entry.memory.memory_id),
        );
        let payload =
            serde_json::to_value(&entry).context("failed to encode task memory sidecar")?;
        let event = self.append_event_if_with_identity(
            DurableEventType::TaskMemoryRecordedV1,
            payload,
            event_id,
            Some(started_event_id.clone()),
            Some(started_event_id),
            |records| {
                let lifecycle = CompactionLifecycleProjection::from_records(records)?;
                let attempt = lifecycle.attempt(attempt_id).with_context(|| {
                    format!("task memory sidecar attempt {attempt_id} is missing")
                })?;
                if attempt.terminal.is_some() {
                    bail!("task memory sidecar attempt {attempt_id} is already terminal");
                }
                entry.validate_shape(&session_id, next_stream_sequence(records))?;
                let sidecars = CompactionSidecarProjection::from_records(records)?;
                if sidecars.recorded_memory(&entry.memory.memory_id).is_some() {
                    bail!(
                        "task memory id {} was recorded more than once",
                        entry.memory.memory_id
                    );
                }
                Ok(true)
            },
        )?;
        event.context("task memory sidecar append was not attempted")
    }

    /// Appends the sole invalidation terminal for an already activated TaskMemory sidecar.
    pub fn append_task_memory_invalidated(
        &self,
        entry: TaskMemoryInvalidatedEntry,
    ) -> Result<StoredEvent> {
        entry.validate_shape()?;
        let records = self.read_event_records_writer()?;
        let sidecars = CompactionSidecarProjection::from_records(&records)?;
        let active = sidecars
            .active_memory(&entry.task_memory_id)
            .with_context(|| format!("task memory {} is not active", entry.task_memory_id))?;
        if entry.invalidated_by_event_id != active.applied_event_id {
            bail!("task memory invalidation source does not match its AppliedV2 event");
        }
        let recorded = sidecars
            .recorded_memory(&entry.task_memory_id)
            .expect("an active sidecar has a recorded task memory");
        let session_id = compaction_session_id(self)?;
        let event_id = compaction_lifecycle_event_id(
            &session_id,
            &recorded.attempt_id,
            &format!("task-memory-invalidated:{}", entry.task_memory_id),
        );
        let root_event_id = recorded.started_event_id.clone();
        let payload =
            serde_json::to_value(&entry).context("failed to encode task memory invalidation")?;
        let event = self.append_event_if_with_identity(
            DurableEventType::TaskMemoryInvalidated,
            payload,
            event_id,
            Some(root_event_id.clone()),
            Some(root_event_id),
            |records| {
                let sidecars = CompactionSidecarProjection::from_records(records)?;
                let active = sidecars
                    .active_memory(&entry.task_memory_id)
                    .with_context(|| {
                        format!("task memory {} is not active", entry.task_memory_id)
                    })?;
                if entry.invalidated_by_event_id != active.applied_event_id {
                    bail!("task memory invalidation source does not match its AppliedV2 event");
                }
                if sidecars.is_invalidated(&entry.task_memory_id) {
                    bail!(
                        "task memory {} was already invalidated",
                        entry.task_memory_id
                    );
                }
                Ok(true)
            },
        )?;
        event.context("task memory invalidation append was not attempted")
    }
}

fn validate_memory_activation(
    attempt: &CompactionAttemptState,
    applied_sequence: u64,
    entry: &CompactionAppliedV2,
    recorded: &RecordedTaskMemory,
) -> Result<()> {
    if recorded.attempt_id != entry.attempt_id
        || recorded.started_event_id != attempt.started_event_id
    {
        bail!("task memory sidecar does not belong to the applied compaction attempt");
    }
    if recorded.stream_sequence >= applied_sequence {
        bail!("task memory sidecar must precede AppliedV2");
    }
    if recorded.entry.derived_through != entry.folded_through {
        bail!("task memory derived cursor does not match AppliedV2 cursor");
    }
    if recorded.entry.memory.branch_id != entry.branch_id {
        bail!("task memory branch does not match AppliedV2 branch");
    }
    if entry.valid_for_snapshot.as_deref()
        != Some(recorded.entry.memory.valid_for_snapshot.as_str())
    {
        bail!("task memory snapshot does not match AppliedV2 snapshot");
    }
    if entry.checkpoint.task_memory_id.as_deref() != Some(recorded.entry.memory.memory_id.as_str())
        || entry.checkpoint.valid_for_snapshot.as_deref()
            != Some(recorded.entry.memory.valid_for_snapshot.as_str())
    {
        bail!("continuation checkpoint does not match activated task memory");
    }
    Ok(())
}

fn validate_portable_checkpoint_activation(
    records: &[SessionStreamRecord],
    entry: &CompactionAppliedV2,
    recorded: &RecordedTaskMemory,
) -> Result<()> {
    if entry.checkpoint.kind != ContinuationCheckpointKind::PortableSemantic {
        return Ok(());
    }
    let source_cursor = entry
        .checkpoint
        .source_plan_cursor
        .as_ref()
        .expect("portable checkpoint shape was validated before activation");
    let source_count = usize::try_from(source_cursor.last_applied_stream_sequence)
        .context("portable checkpoint source plan cursor overflows usize")?;
    if source_count >= recorded.stream_sequence as usize {
        bail!("portable checkpoint source plan must precede its task memory sidecar");
    }
    let source_tail = records
        .get(source_count.saturating_sub(1))
        .context("portable checkpoint source plan cursor is missing")?;
    if source_tail.projection_cursor(COMPACTION_FOLD_PLAN_SCHEMA_VERSION) != *source_cursor {
        bail!("portable checkpoint source plan cursor does not match raw history");
    }
    let source_records = &records[..source_count];
    let plan = CompactionFoldPlan::from_records_after(
        source_records,
        entry
            .checkpoint
            .requested_tail_message_count
            .expect("portable checkpoint shape was validated before activation"),
        entry.checkpoint.prior_folded_through.as_ref(),
    )?;
    if plan.base_stream_cursor != *source_cursor
        || plan.folded_through.as_ref() != Some(&entry.folded_through)
    {
        bail!("portable checkpoint does not match its deterministic fold plan");
    }
    let catalog = ContinuationSourceCatalog::from_fold_plan(source_records, &plan)?;
    entry
        .checkpoint
        .validate_for_activation(&recorded.entry.memory, &catalog)
}

fn ensure_empty_checkpoint(entry: &CompactionAppliedV2) -> Result<()> {
    if entry.checkpoint.task_memory_id.is_some() || entry.checkpoint.valid_for_snapshot.is_some() {
        bail!("AppliedV2 without task memory must use an empty checkpoint binding");
    }
    Ok(())
}

fn validate_sidecar_lineage(event: &StoredEvent, attempt: &CompactionAttemptState) -> Result<()> {
    if event.correlation_id.as_deref() != Some(attempt.started_event_id.as_str())
        || event.causation_id.as_deref() != Some(attempt.started_event_id.as_str())
    {
        bail!("task memory sidecar must remain in its compaction start lineage");
    }
    Ok(())
}

fn task_memory_identity(memory: &TaskMemoryV1) -> Result<(String, u64)> {
    let value = serde_json::to_value(memory).context("failed to encode task memory payload")?;
    let canonical = crate::event::canonical_json_bytes(&value)?;
    Ok((
        crate::event::canonical_json_content_hash(&value)?,
        canonical.len() as u64,
    ))
}

fn decode_sidecar_payload<T>(event: &StoredEvent) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value::<T>(event.payload.clone())
        .with_context(|| format!("failed to decode {} sidecar payload", event.event_type))
}

#[cfg(test)]
#[path = "tests/compaction_sidecar_tests.rs"]
mod tests;
