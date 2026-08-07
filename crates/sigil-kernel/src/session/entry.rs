use super::*;
use crate::{
    ApprovalRequestIdentityV2, CommandPermissionMatch, ExecutionContainmentBindingV2,
    PermissionDecisionReason, ToolPermissionEffect, ToolSemanticScope,
    integration::{
        IntegrationLaneChanged, IntegrationLaneCleanupRecorded, IntegrationLaneMemberApplied,
        IntegrationLanePrepared, IntegrationLaneTerminal, IntegrationLaneVerificationLinked,
        IntegrationPlanRecorded, IntegrationPromotionRecorded, TaskParentVerificationRecorded,
        TaskPromotionAuthorityConsumed, TaskPromotionPreviewRecorded,
    },
};

/// Append-only session log entry stored in the durable JSONL session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // Keep the durable JSONL shape unboxed across control variants.
#[serde(rename_all = "snake_case")]
pub enum SessionLogEntry {
    User(ModelMessage),
    Assistant(ModelMessage),
    /// Artifact-backed result used by all new tool executions (RFC-0062 V3 contract).
    ToolResultV3(ToolResultRecordedV3),
    Control(ControlEntry),
}

/// One v2 durable record in a session stream.
#[derive(Debug, Clone)]
pub enum SessionStreamRecord {
    Stored(StoredEvent),
}

impl SessionStreamRecord {
    /// Returns the v2 durable envelope carried by this stream record.
    pub fn stored_event(&self) -> &StoredEvent {
        let Self::Stored(event) = self;
        event
    }

    /// Consumes this record and returns its v2 durable envelope.
    pub fn into_stored_event(self) -> StoredEvent {
        let Self::Stored(event) = self;
        event
    }

    pub fn stream_sequence(&self) -> u64 {
        match self {
            Self::Stored(event) => event.stream_sequence,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::Stored(event) => &event.session_id,
        }
    }

    pub fn event_id(&self) -> &str {
        match self {
            Self::Stored(event) => &event.event_id,
        }
    }

    pub fn record_checksum(&self) -> &str {
        match self {
            Self::Stored(event) => &event.record_checksum,
        }
    }

    pub fn projection_cursor(&self, projection_schema_version: u16) -> ProjectionCursor {
        ProjectionCursor {
            session_id: self.session_id().to_owned(),
            projection_schema_version,
            last_applied_stream_sequence: self.stream_sequence(),
            last_applied_event_id: self.event_id().to_owned(),
            last_applied_record_checksum: self.record_checksum().to_owned(),
        }
    }

    pub fn domain_event_record(&self) -> Result<Option<DomainEventRecord>> {
        let domain_event = match self {
            Self::Stored(event) => match decode_stored_event(event.clone())? {
                StoredEventDecode::Known(event) => Some(event),
                StoredEventDecode::UnknownNonCritical(_) => None,
            },
        };
        Ok(domain_event.map(|event| DomainEventRecord {
            event,
            cursor: self.projection_cursor(SESSION_ENTRY_PROJECTION_SCHEMA_VERSION),
        }))
    }

    /// Decodes the canonical session-log entry carried by this validated durable envelope.
    ///
    /// Callers outside the kernel should use this method instead of inspecting raw event JSON.
    /// Unknown recovery-critical events, checksum failures, and malformed embedded entries fail
    /// closed; known events without a session-log projection return `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when the envelope or its canonical session entry is invalid.
    pub fn session_log_entry(&self) -> Result<Option<SessionLogEntry>> {
        let Some(record) = self.domain_event_record()? else {
            return Ok(None);
        };
        super::session_entry_from_domain_event(&record.event)
    }

    pub fn typed_domain_event_record(&self) -> Result<Option<TypedDomainEventRecord>> {
        match self {
            Self::Stored(event) => {
                let typed_event = match decode_typed_stored_event(event.clone())? {
                    TypedStoredEventDecode::Known(event) => Some(*event),
                    TypedStoredEventDecode::UnknownNonCritical(_) => None,
                };
                Ok(typed_event.map(|event| TypedDomainEventRecord {
                    event,
                    cursor: self.projection_cursor(SESSION_ENTRY_PROJECTION_SCHEMA_VERSION),
                }))
            }
        }
    }
}

pub const SESSION_ENTRY_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_THREAD_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_PROFILE_TRUST_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_PROFILE_POLICY_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_RESULT_CONTINUATION_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const CHANGESET_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const CONVERSATION_QUEUE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const PLAN_ARTIFACT_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const PLUGIN_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const SKILL_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const TASK_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const TERMINAL_TASK_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const USAGE_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const WRITE_ISOLATION_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const VERIFICATION_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;

/// One reducer-facing domain event plus the cursor position proving where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainEventRecord {
    pub event: DomainEvent,
    pub cursor: ProjectionCursor,
}

/// One strongly typed reducer-facing v2 event plus the cursor position proving its source.
#[derive(Debug, Clone)]
pub struct TypedDomainEventRecord {
    pub event: TypedDomainEvent,
    pub cursor: ProjectionCursor,
}

/// Stable memory payload captured for a specific memory fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemorySnapshot {
    pub messages: Vec<ModelMessage>,
    pub report: MemoryLoadReport,
}

/// Audit entry recorded when Context V1 candidates are recalled but cannot be rendered safely.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ContextAssemblySkippedEntry {
    pub reason: String,
    pub candidate_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub item_ids: Vec<ContextItemId>,
}

/// Control-plane state that must survive resume and remain outside model-facing chat history.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // Boxing variants would churn append-only control projection matches.
#[serde(rename_all = "snake_case")]
pub enum ControlEntry {
    SessionIdentity {
        provider_name: String,
        model_name: String,
        resolved_model_route: Option<crate::ResolvedModelRoute>,
    },
    SessionModelSelected {
        provider_name: String,
        model_name: String,
        resolved_model_route: crate::ResolvedModelRoute,
    },
    SessionRouteRebound {
        provider_name: String,
        model_name: String,
        resolved_model_route: crate::ResolvedModelRoute,
    },
    SessionRouteTrustBound {
        route_semantic_fingerprint: String,
        egress_trust_binding: crate::RouteEgressTrustBinding,
    },
    ContinuationStateSaved(ProviderContinuationState),
    ResponseHandleTracked(crate::provider::ResponseHandle),
    BackgroundTaskTracked(crate::provider::BackgroundTaskHandle),
    PrefixSnapshotCaptured(PrefixSnapshot),
    MemorySnapshotCaptured(MemorySnapshot),
    ContextAssemblySkipped(ContextAssemblySkippedEntry),
    ExternalProvenance(ExternalProvenanceEntry),
    WebUrlCapabilityDescriptor(crate::WebUrlCapabilityDescriptor),
    UsageSnapshot(UsageStats),
    /// Usage emitted by the internal semantic-compaction model request.
    ///
    /// It contributes to total session cost but must not replace the latest ordinary
    /// conversation-generation usage used for current-epoch cache forecasting.
    SemanticCompactionUsageSnapshot(UsageStats),
    ToolApproval(ToolApprovalEntry),
    ToolPermissionPlannedV2(Box<ToolPermissionPlannedV2Entry>),
    ToolPermissionDecisionV2(Box<ToolPermissionDecisionV2Entry>),
    ToolApprovalSessionGrant(ToolApprovalSessionGrantEntry),
    ToolExecution(Box<ToolExecutionEntry>),
    ToolArtifactRead(ToolArtifactReadRecordedV1),
    /// RFC-0062 9.4: generation-guarded availability transition for one artifact.
    ToolArtifactAvailabilityChanged(ToolArtifactAvailabilityChangedV1),
    /// RFC-0062 16.2: durable tombstone intent persisted before the physical body move so a
    /// crash between the move and the terminal Expired append can be reconciled on restart.
    ToolArtifactTombstonePlan(ToolArtifactTombstonePlannedV1),
    ToolEgress(Box<ToolEgressEntry>),
    McpElicitation(Box<McpElicitationEntry>),
    ToolPreviewCaptured(ToolPreviewSnapshot),
    SkillIndexCaptured(SkillIndexSnapshot),
    SkillLoaded(SkillLoadEntry),
    PluginManifestCaptured(PluginManifestSnapshot),
    PluginTrustDecision(PluginTrustEntry),
    PluginHookExecutionStarted(PluginHookExecutionStartedEntry),
    PluginHookExecutionFinished(PluginHookExecutionFinishedEntry),
    ChangeSetProposed(ChangeSet),
    ChangeSetApplied(ChangeSetResult),
    TerminalTask(TerminalTaskEntry),
    ConversationRouteDecisionRecorded(crate::ConversationRouteDecisionRecordedEntry),
    PlanReviewAttempt(crate::PlanReviewAttemptEntry),
    PlanDraftCreated(PlanDraftCreatedEntry),
    PlanDecisionRecorded(PlanDecisionRecordedEntry),
    PlanPermissionGranted(PlanPermissionGrantedEntry),
    TaskCreatedFromPlan(TaskCreatedFromPlanEntry),
    TaskHandoffRequested(TaskHandoffRequestedEntry),
    TaskHandoffResolved(TaskHandoffResolvedEntry),
    TaskRun(TaskRunEntry),
    TaskRunCancellationScopeBound(TaskRunCancellationScopeBoundEntry),
    TaskPlan(TaskPlanEntry),
    TaskGuidanceApplied(crate::TaskGuidanceAppliedEntry),
    TaskStep(TaskStepEntry),
    TaskParticipantAttempt(TaskParticipantAttemptEntry),
    TaskParticipantRetryScheduled(TaskParticipantRetryScheduledEntry),
    TaskParticipantResult(TaskParticipantResultEntry),
    TaskFinalAnswerCommitted(TaskFinalAnswerCommittedEntry),
    OrchestrationRouteDisabled(crate::OrchestrationRouteDisabledEntry),
    TaskChildSession(TaskChildSessionEntry),
    TaskChildSessionDisplayName(TaskChildSessionDisplayNameEntry),
    TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry),
    TaskSubagentElicitationRoute(TaskSubagentElicitationRouteEntry),
    JobIntentRecorded(crate::resume::JobIntentEntry),
    StepLeaseRecorded(crate::resume::StepLeaseEntry),
    StepLeaseHeartbeatRecorded(crate::resume::StepLeaseHeartbeatEntry),
    CheckSpecRecorded(CheckSpecRecordedEntry),
    VerificationPolicyChanged(VerificationPolicyChangedEntry),
    VerificationCheckRun(VerificationCheckRunEntry),
    VerificationRecorded(VerificationRecordedEntry),
    VerificationReceiptLinkRecorded(VerificationReceiptLinkRecorded),
    VerificationFailureLocatorRecorded(VerificationFailureLocatorRecorded),
    ReadinessEvaluated(ReadinessEvaluatedEntry),
    ChildVerificationReceiptLinked(ChildVerificationReceiptLinked),
    WorkspaceTrustDecision(WorkspaceTrustDecisionEntry),
    WriteLeaseAcquired(WriteLeaseAcquired),
    WriteLeaseReleased(WriteLeaseReleased),
    IsolatedWorkspacePrepared(IsolatedWorkspacePrepared),
    IsolatedWorkspaceCreated(IsolatedWorkspaceCreated),
    IsolatedWorkspaceCleanupRecorded(IsolatedWorkspaceCleanupRecorded),
    IsolatedChangeSetProduced(IsolatedChangeSetProduced),
    MergeReviewRequested(MergeReviewRequested),
    MergeReviewResolved(MergeReviewResolved),
    IntegrationPlanRecorded(IntegrationPlanRecorded),
    IntegrationLaneChanged(IntegrationLaneChanged),
    IntegrationLanePrepared(IntegrationLanePrepared),
    IntegrationLaneMemberApplied(IntegrationLaneMemberApplied),
    IntegrationLaneVerificationLinked(IntegrationLaneVerificationLinked),
    IntegrationLaneTerminal(IntegrationLaneTerminal),
    IntegrationLaneCleanupRecorded(IntegrationLaneCleanupRecorded),
    TaskPromotionPreviewRecorded(TaskPromotionPreviewRecorded),
    TaskPromotionAuthorityConsumed(TaskPromotionAuthorityConsumed),
    IntegrationPromotionRecorded(IntegrationPromotionRecorded),
    TaskParentVerificationRecorded(TaskParentVerificationRecorded),
    AgentProfileCaptured(AgentProfileCapturedEntry),
    AgentProfileTrustDecision(AgentProfileTrustEntry),
    AgentProfilePolicyDecision(AgentProfilePolicyEntry),
    AgentDelegationAdmitted(AgentDelegationAdmissionEntry),
    AgentThreadStarted(AgentThreadStartedEntry),
    AgentThreadStatusChanged(AgentThreadStatusChangedEntry),
    AgentThreadMessageRouted(AgentThreadMessageRoutedEntry),
    AgentMailboxMessage(AgentMailboxMessageEntry),
    AgentThreadResultRecorded(AgentThreadResultRecordedEntry),
    AgentThreadResultDelivered(AgentThreadResultDeliveredEntry),
    AgentResultContinuation(AgentResultContinuationEntry),
    AgentThreadDisplayName(AgentThreadDisplayNameEntry),
    AgentApprovalRoute(AgentApprovalRouteEntry),
    AgentElicitationRoute(AgentElicitationRouteEntry),
    AgentRunAttemptStarted(AgentRunAttemptStartedEntry),
    AgentRunHeartbeat(AgentRunHeartbeatEntry),
    AgentRunInterrupted(AgentRunInterruptedEntry),
    AgentRouteClosed(AgentRouteClosedEntry),
    AgentMergeSafePoint(AgentMergeSafePointEntry),
    AgentThreadClosed(AgentThreadClosedEntry),
    ConversationInputQueued(ConversationInputQueuedEntry),
    ConversationInputQueueControl(ConversationInputQueueControlEntry),
    ConversationInputEdited(ConversationInputEditedEntry),
    ConversationInputReordered(ConversationInputReorderedEntry),
    ConversationInputStatusChanged(ConversationInputStatusEntry),
    /// Audit projection of the critical direct queue-promotion event. The safe user message is
    /// bound here for replay, but does not become provider-visible until the pre-turn sender
    /// consumes the promotion contract.
    ConversationInputPromoted(crate::ConversationInputPromotedEntry),
    /// Scheduler-owned binding of queued task guidance to one accepted task plan version.
    TaskGuidancePromoted(crate::TaskGuidancePromotedEntry),
    Note {
        kind: String,
        data: serde_json::Value,
    },
}

impl ControlEntry {
    /// Validates high-risk V2 control payloads before append and after decode.
    ///
    /// These entries must stay well below the generic event ceiling so an untrusted tool call or
    /// policy configuration cannot recreate an oversized session record.
    pub(crate) fn validate_durable_contract(&self) -> anyhow::Result<()> {
        match self {
            Self::ToolPermissionPlannedV2(entry) => entry.validate(),
            Self::ToolPermissionDecisionV2(entry) => entry.validate(),
            Self::ToolApproval(entry) => entry.validate(),
            Self::ToolApprovalSessionGrant(entry) => entry.validate(),
            Self::ToolExecution(entry) => entry.validate(),
            Self::TerminalTask(entry) => entry.validate_durable(),
            Self::ToolArtifactAvailabilityChanged(entry) => entry.validate(),
            Self::ToolArtifactTombstonePlan(entry) => entry.validate(),
            _ => Ok(()),
        }
    }
}

/// Recovery-safe, secret-free permission plan recorded before policy evaluation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolPermissionPlannedV2Entry {
    pub schema_version: u32,
    pub call_id: String,
    pub tool_name: String,
    pub plan_hash: String,
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub effects: std::collections::BTreeSet<ToolPermissionEffect>,
    pub analysis: crate::ToolAnalysisStatus,
    pub containment: crate::ExecutionContainmentRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_scope: Option<ToolSemanticScope>,
    pub subjects: Vec<ToolSubjectAudit>,
    pub subject_count: u32,
    pub subjects_sha256: String,
    pub analysis_binding_names: Vec<String>,
    pub analysis_bindings_sha256: String,
    pub safe_summary: crate::ToolPermissionSummary,
}

pub const TOOL_PERMISSION_PLANNED_AUDIT_SCHEMA_VERSION: u32 = 2;
pub const MAX_DURABLE_TOOL_CONTROL_BYTES: usize = 64 * 1024;
pub const MAX_DURABLE_PERMISSION_MATCHES: usize = 32;
pub const MAX_DURABLE_PERMISSION_REASONS: usize = 32;
pub const MAX_DURABLE_PERMISSION_TEXT_BYTES: usize = 512;
pub const MAX_DURABLE_SUBJECT_LABEL_BYTES: usize = 256;

impl ToolPermissionPlannedV2Entry {
    /// Creates the bounded recovery projection of one immutable live permission plan.
    pub fn from_plan(call_id: &str, plan: &crate::ToolPermissionPlanV2) -> anyhow::Result<Self> {
        let subjects = plan
            .subjects
            .iter()
            .map(ToolSubjectAudit::from)
            .collect::<Vec<_>>();
        let subject_count = subjects
            .len()
            .try_into()
            .map_err(|_| anyhow::anyhow!("permission audit subject count exceeds u32"))?;
        let subjects_value = serde_json::to_value(&subjects)
            .context("failed to project permission audit subjects")?;
        let subjects_sha256 = crate::stable_event_hash(
            &crate::event::canonical_json_bytes(&subjects_value)
                .context("failed to encode permission audit subjects")?,
        );
        let analysis_binding_names = plan.analysis_bindings.keys().cloned().collect::<Vec<_>>();
        let analysis_bindings_value = serde_json::to_value(&plan.analysis_bindings)
            .context("failed to project permission analysis bindings")?;
        let analysis_bindings_sha256 = crate::stable_event_hash(
            &crate::event::canonical_json_bytes(&analysis_bindings_value)
                .context("failed to encode permission analysis bindings")?,
        );
        let entry = Self {
            schema_version: TOOL_PERMISSION_PLANNED_AUDIT_SCHEMA_VERSION,
            call_id: call_id.to_owned(),
            tool_name: plan.tool_name.clone(),
            plan_hash: plan.plan_hash.clone(),
            access: plan.access,
            operation: plan.operation,
            effects: plan.effects.clone(),
            analysis: plan.analysis.clone(),
            containment: plan.containment.clone(),
            semantic_scope: plan.semantic_scope.clone(),
            subjects,
            subject_count,
            subjects_sha256,
            analysis_binding_names,
            analysis_bindings_sha256,
            safe_summary: plan.safe_summary.clone(),
        };
        entry.validate()?;
        Ok(entry)
    }

    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != TOOL_PERMISSION_PLANNED_AUDIT_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported tool permission planned audit schema version {}",
                self.schema_version
            );
        }
        validate_durable_identity("permission audit call id", &self.call_id)?;
        validate_durable_identity("permission audit tool name", &self.tool_name)?;
        validate_sha256("permission audit plan hash", &self.plan_hash)?;
        crate::permission_plan::validate_analysis_status(&self.analysis)?;
        if let Some(scope) = &self.semantic_scope {
            crate::permission_plan::validate_semantic_scope(scope)?;
        }
        if self.subjects.len() > crate::MAX_TOOL_PERMISSION_SUBJECTS
            || usize::try_from(self.subject_count).ok() != Some(self.subjects.len())
        {
            anyhow::bail!("permission audit subject count is invalid");
        }
        self.subjects
            .iter()
            .try_for_each(ToolSubjectAudit::validate)?;
        validate_sha256("permission audit subjects hash", &self.subjects_sha256)?;
        if self.analysis_binding_names.len() > crate::MAX_TOOL_ANALYSIS_BINDINGS {
            anyhow::bail!("permission audit analysis binding names exceed maximum count");
        }
        for name in &self.analysis_binding_names {
            validate_bounded_safe_text(
                "permission audit analysis binding name",
                name,
                crate::MAX_TOOL_ANALYSIS_BINDING_KEY_BYTES,
            )?;
        }
        validate_sha256(
            "permission audit analysis bindings hash",
            &self.analysis_bindings_sha256,
        )?;
        validate_bounded_safe_text(
            "permission audit summary title",
            &self.safe_summary.title,
            crate::MAX_TOOL_PERMISSION_SUMMARY_TITLE_BYTES,
        )?;
        validate_bounded_safe_text(
            "permission audit summary detail",
            &self.safe_summary.detail,
            crate::MAX_TOOL_PERMISSION_SUMMARY_DETAIL_BYTES,
        )?;
        validate_serialized_tool_control_size(self)
    }
}

/// Recovery-safe result of evaluating one immutable permission plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolPermissionDecisionV2Entry {
    pub schema_version: u32,
    pub call_id: String,
    pub tool_name: String,
    pub plan_hash: String,
    pub policy_version: String,
    pub policy_decision: ApprovalMode,
    pub access: ToolAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_effect: Option<NetworkEffect>,
    pub local_policy_decision: ApprovalMode,
    pub network_policy_decision: ApprovalMode,
    pub source_policy_decision: ApprovalMode,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subjects: Vec<ToolSubjectAudit>,
    pub subject_zones: Vec<PathTrustZone>,
    pub external_directory_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<PermissionConfirmation>,
    pub snapshot_required: bool,
    pub command_permission_matches: Vec<CommandPermissionMatch>,
    pub decision_reasons: Vec<PermissionDecisionReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_source: Option<ToolApprovalAllowSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_digest: Option<String>,
}

impl ToolPermissionDecisionV2Entry {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != TOOL_PERMISSION_DECISION_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported tool permission decision schema version {}",
                self.schema_version
            );
        }
        validate_durable_identity("permission decision call id", &self.call_id)?;
        validate_durable_identity("permission decision tool name", &self.tool_name)?;
        validate_sha256("permission decision plan hash", &self.plan_hash)?;
        validate_bounded_safe_text(
            "permission decision policy version",
            &self.policy_version,
            MAX_DURABLE_PERMISSION_TEXT_BYTES,
        )?;
        validate_permission_audit_collections(
            &self.subjects,
            &self.command_permission_matches,
            &self.decision_reasons,
        )?;
        if self.subject_zones.len() > crate::MAX_TOOL_PERMISSION_SUBJECTS {
            anyhow::bail!("permission decision subject zones exceed maximum count");
        }
        if let Some(PermissionConfirmation::TypePhrase { phrase }) = &self.confirmation {
            validate_sha256("permission decision confirmation phrase hash", phrase)?;
        }
        if let Some(grant_id) = self.grant_id.as_deref() {
            validate_durable_identity("permission decision session grant id", grant_id)?;
        }
        if let Some(prepared_digest) = self.prepared_digest.as_deref() {
            validate_sha256("permission decision prepared digest", prepared_digest)?;
        }
        validate_serialized_tool_control_size(self)
    }
}

pub const TOOL_PERMISSION_DECISION_SCHEMA_VERSION: u32 = 2;

/// Append-only audit entry for permission policy evaluation and interactive approval decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolApprovalEntry {
    pub schema_version: u32,
    pub identity: ApprovalRequestIdentityV2,
    pub plan_hash: String,
    pub action: ToolApprovalAuditAction,
    pub call_id: String,
    pub tool_name: String,
    pub access: ToolAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_effect: Option<NetworkEffect>,
    pub local_policy_decision: ApprovalMode,
    pub network_policy_decision: ApprovalMode,
    pub source_policy_decision: ApprovalMode,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subjects: Vec<ToolSubjectAudit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_zones: Vec<PathTrustZone>,
    pub policy_decision: ApprovalMode,
    pub external_directory_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<PermissionConfirmation>,
    pub snapshot_required: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command_permission_matches: Vec<CommandPermissionMatch>,
    pub decision_reasons: Vec<PermissionDecisionReason>,
    pub user_decision: Option<ToolApprovalUserDecision>,
    pub reason: Option<String>,
    pub preview_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_receipt: Option<ToolApprovalDecisionReceiptV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_status: Option<ToolApprovalTerminalStatusV2>,
}

impl ToolApprovalEntry {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != TOOL_APPROVAL_AUDIT_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported tool approval audit schema version {}",
                self.schema_version
            );
        }
        validate_approval_identity(&self.identity)?;
        validate_sha256("tool approval plan hash", &self.plan_hash)?;
        if self.identity.call_id != self.call_id || self.identity.plan_hash != self.plan_hash {
            anyhow::bail!("tool approval durable identity is inconsistent");
        }
        validate_durable_identity("tool approval call id", &self.call_id)?;
        validate_durable_identity("tool approval tool name", &self.tool_name)?;
        validate_permission_audit_collections(
            &self.subjects,
            &self.command_permission_matches,
            &self.decision_reasons,
        )?;
        if self.subject_zones.len() > crate::MAX_TOOL_PERMISSION_SUBJECTS {
            anyhow::bail!("tool approval subject zones exceed maximum count");
        }
        if let Some(PermissionConfirmation::TypePhrase { phrase }) = &self.confirmation {
            validate_sha256("tool approval confirmation phrase hash", phrase)?;
        }
        if let Some(reason) = self.reason.as_deref() {
            validate_bounded_safe_text(
                "tool approval reason",
                reason,
                MAX_DURABLE_PERMISSION_TEXT_BYTES,
            )?;
        }
        if let Some(preview_hash) = self.preview_hash.as_deref() {
            validate_sha256("tool approval preview hash", preview_hash)?;
        }
        if let Some(receipt) = &self.decision_receipt {
            validate_durable_identity(
                "tool approval decision receipt id",
                &receipt.approval_request_id,
            )?;
            if receipt.approval_request_id != self.identity.approval_request_id {
                anyhow::bail!("tool approval decision receipt identity is inconsistent");
            }
        }
        self.validate_phase_contract()?;
        validate_serialized_tool_control_size(self)
    }

    fn validate_phase_contract(&self) -> anyhow::Result<()> {
        match self.action {
            ToolApprovalAuditAction::Requested | ToolApprovalAuditAction::PreviewFailed => {
                if self.user_decision.is_some()
                    || self.decision_receipt.is_some()
                    || self.terminal_status.is_some()
                {
                    anyhow::bail!(
                        "non-decision tool approval phase contains decision or terminal state"
                    );
                }
            }
            ToolApprovalAuditAction::DecisionAccepted => {
                let user_decision = self.user_decision.ok_or_else(|| {
                    anyhow::anyhow!("accepted tool approval decision is missing user decision")
                })?;
                let receipt = self.decision_receipt.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("accepted tool approval decision is missing decision receipt")
                })?;
                if receipt.decision != user_decision {
                    anyhow::bail!("tool approval decision receipt does not match user decision");
                }
                if self.terminal_status.is_some() {
                    anyhow::bail!("accepted tool approval decision must not be terminal");
                }
            }
            ToolApprovalAuditAction::Resolved => {
                if self.decision_receipt.is_some() {
                    anyhow::bail!("resolved tool approval must not repeat decision receipt");
                }
                let terminal_status = self.terminal_status.ok_or_else(|| {
                    anyhow::anyhow!("resolved tool approval is missing terminal status")
                })?;
                let consistent = matches!(
                    (terminal_status, self.user_decision),
                    (
                        ToolApprovalTerminalStatusV2::Approved,
                        Some(ToolApprovalUserDecision::Approved)
                    ) | (
                        ToolApprovalTerminalStatusV2::ApprovedForSession,
                        Some(ToolApprovalUserDecision::ApprovedForSession)
                    ) | (
                        ToolApprovalTerminalStatusV2::Denied,
                        Some(ToolApprovalUserDecision::Denied) | None
                    ) | (
                        ToolApprovalTerminalStatusV2::Expired
                            | ToolApprovalTerminalStatusV2::Stale
                            | ToolApprovalTerminalStatusV2::Cancelled,
                        None
                    )
                );
                if !consistent {
                    anyhow::bail!(
                        "resolved tool approval terminal status does not match user decision"
                    );
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
impl ToolApprovalEntry {
    pub(crate) fn test_fixture(
        action: ToolApprovalAuditAction,
        call_id: impl Into<String>,
        tool_name: impl Into<String>,
    ) -> Self {
        let call_id = call_id.into();
        let plan_hash = crate::stable_event_hash(b"test-plan");
        let (user_decision, decision_receipt, terminal_status) = match action {
            ToolApprovalAuditAction::Requested | ToolApprovalAuditAction::PreviewFailed => {
                (None, None, None)
            }
            ToolApprovalAuditAction::DecisionAccepted => {
                let decision = ToolApprovalUserDecision::Approved;
                (
                    Some(decision),
                    Some(ToolApprovalDecisionReceiptV2 {
                        approval_request_id: format!("approval-{call_id}"),
                        decision,
                        accepted_at_ms: 1_000,
                    }),
                    None,
                )
            }
            ToolApprovalAuditAction::Resolved => (
                Some(ToolApprovalUserDecision::Approved),
                None,
                Some(ToolApprovalTerminalStatusV2::Approved),
            ),
        };
        Self {
            schema_version: TOOL_APPROVAL_AUDIT_SCHEMA_VERSION,
            identity: ApprovalRequestIdentityV2 {
                session_id: "test-session".to_owned(),
                run_id: "test-run".to_owned(),
                call_id: call_id.clone(),
                approval_request_id: format!("approval-{call_id}"),
                plan_hash: plan_hash.clone(),
                policy_version: crate::stable_event_hash(b"test-policy"),
                execution_binding_hash: plan_hash.clone(),
                expires_at_ms: 1_000,
            },
            plan_hash,
            action,
            call_id,
            tool_name: tool_name.into(),
            access: ToolAccess::Read,
            network_effect: None,
            local_policy_decision: ApprovalMode::Ask,
            network_policy_decision: ApprovalMode::Allow,
            source_policy_decision: ApprovalMode::Allow,
            operation: ToolOperation::Read,
            risk: PermissionRisk::Low,
            subjects: Vec::new(),
            subject_zones: Vec::new(),
            policy_decision: ApprovalMode::Ask,
            external_directory_required: false,
            confirmation: None,
            snapshot_required: false,
            command_permission_matches: Vec::new(),
            decision_reasons: Vec::new(),
            user_decision,
            reason: None,
            preview_hash: None,
            decision_receipt,
            terminal_status,
        }
    }
}

pub const TOOL_APPROVAL_AUDIT_SCHEMA_VERSION: u32 = 2;

/// Source that allowed a tool call after policy evaluation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalAllowSource {
    SessionGrant,
}

/// Stable phase marker for one approval audit entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalAuditAction {
    Requested,
    DecisionAccepted,
    Resolved,
    PreviewFailed,
}

/// Stable user approval decision persisted in the control log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalUserDecision {
    Approved,
    ApprovedForSession,
    Denied,
}

/// Kernel acknowledgement that the exact approval route returned a bound decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolApprovalDecisionReceiptV2 {
    pub approval_request_id: String,
    pub decision: ToolApprovalUserDecision,
    pub accepted_at_ms: u64,
}

/// Unique terminal outcome for one durable approval request.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalTerminalStatusV2 {
    Approved,
    ApprovedForSession,
    Denied,
    Expired,
    Stale,
    Cancelled,
}

/// Append-only session-local approval grant created from an interactive tool approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ToolApprovalSessionGrantEntry {
    pub schema_version: u32,
    pub grant_id: String,
    pub source_call_id: String,
    pub source_approval_request_id: String,
    pub tool_name: String,
    pub semantic_scope: ToolSemanticScope,
    pub effect_ceiling: std::collections::BTreeSet<ToolPermissionEffect>,
    pub risk_ceiling: PermissionRisk,
    pub subjects: Vec<ToolSubjectAudit>,
    pub facets: Vec<ToolApprovalSessionGrantFacet>,
    pub scope: ToolApprovalSessionGrantScope,
    pub containment_binding: ExecutionContainmentBindingV2,
    pub policy_version: String,
    pub expires: ToolApprovalSessionGrantExpiry,
    pub granted_at_ms: u64,
}

impl ToolApprovalSessionGrantEntry {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION {
            anyhow::bail!(
                "unsupported tool approval session grant schema version {}",
                self.schema_version
            );
        }
        for (label, value) in [
            ("session grant id", self.grant_id.as_str()),
            ("session grant source call id", self.source_call_id.as_str()),
            (
                "session grant approval request id",
                self.source_approval_request_id.as_str(),
            ),
            ("session grant tool name", self.tool_name.as_str()),
        ] {
            validate_durable_identity(label, value)?;
        }
        crate::permission_plan::validate_semantic_scope(&self.semantic_scope)?;
        if self.subjects.len() > crate::MAX_TOOL_PERMISSION_SUBJECTS {
            anyhow::bail!("session grant subjects exceed maximum count");
        }
        self.subjects
            .iter()
            .try_for_each(ToolSubjectAudit::validate)?;
        if self.facets.len() > 4 {
            anyhow::bail!("session grant facets exceed maximum count");
        }
        validate_bounded_safe_text(
            "session grant policy version",
            &self.policy_version,
            MAX_DURABLE_PERMISSION_TEXT_BYTES,
        )?;
        validate_sha256(
            "session grant backend identity hash",
            &self.containment_binding.backend_identity_hash,
        )?;
        validate_sha256(
            "session grant backend profile hash",
            &self.containment_binding.backend_profile_hash,
        )?;
        validate_sha256(
            "session grant environment binding hash",
            &self.containment_binding.environment_binding_hash,
        )?;
        if let ToolApprovalSessionGrantScope::CommandFamily { prefix } = &self.scope
            && (prefix.trim().is_empty() || prefix.contains('\n') || prefix.contains('\r'))
        {
            anyhow::bail!("session grant command family prefix must be non-empty and one line");
        }
        validate_serialized_tool_control_size(self)
    }
}

pub const TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION: u32 = 2;

/// Expiration policy for a session-local tool approval grant.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolApprovalSessionGrantExpiry {
    Session,
}

/// Append-only audit entry for one tool execution lifecycle step.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolExecutionEntry {
    pub call_id: String,
    pub tool_name: String,
    pub status: ToolExecutionStatus,
    pub duration_ms: Option<u64>,
    pub subjects: Vec<ToolSubjectAudit>,
    pub changed_files: Vec<String>,
    pub metadata: ToolResultMeta,
    pub error: Option<ToolError>,
    pub model_content_hash: Option<String>,
}

pub const MAX_DURABLE_TOOL_EXECUTION_BYTES: usize = 64 * 1024;
pub const MAX_DURABLE_TOOL_EXECUTION_DETAILS_BYTES: usize = 24 * 1024;
pub const MAX_DURABLE_TOOL_EXECUTION_PATHS: usize = 64;
pub const MAX_DURABLE_TOOL_EXECUTION_RECEIPT_IDS: usize = 64;

impl ToolExecutionEntry {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        crate::persistence::validate_tool_call_id(&self.call_id)
            .map_err(|error| anyhow::anyhow!(error))?;
        crate::persistence::validate_tool_call_name(&self.tool_name)
            .map_err(|error| anyhow::anyhow!(error))?;
        validate_permission_audit_collections(&self.subjects, &[], &[])?;
        validate_durable_execution_paths(&self.changed_files)?;
        validate_durable_execution_paths(&self.metadata.changed_files)?;
        if let Some(receipt) = &self.metadata.receipt {
            if receipt.mutation_operation_ids.len() > MAX_DURABLE_TOOL_EXECUTION_RECEIPT_IDS {
                anyhow::bail!("tool execution receipt ids exceed maximum count");
            }
            if let Some(key) = &receipt.idempotency_key {
                validate_sha256("tool execution receipt idempotency hash", key)?;
            }
            for operation_id in &receipt.mutation_operation_ids {
                validate_bounded_safe_text(
                    "tool execution mutation operation id",
                    operation_id,
                    MAX_DURABLE_PERMISSION_TEXT_BYTES,
                )?;
            }
        }
        validate_durable_execution_details(&self.metadata.details)?;
        if let Some(error) = &self.error {
            validate_bounded_safe_text(
                "tool execution error message",
                &error.message,
                MAX_DURABLE_PERMISSION_TEXT_BYTES,
            )?;
            validate_durable_execution_error_details(&error.details)?;
        }
        if let Some(hash) = &self.model_content_hash {
            validate_sha256("tool execution model content hash", hash)?;
        }
        let bytes =
            serde_json::to_vec(self).context("failed to size durable tool execution audit")?;
        if bytes.len() > MAX_DURABLE_TOOL_EXECUTION_BYTES {
            anyhow::bail!(
                "durable tool execution exceeds maximum of {} bytes",
                MAX_DURABLE_TOOL_EXECUTION_BYTES
            );
        }
        Ok(())
    }
}

fn validate_durable_execution_paths(paths: &[String]) -> anyhow::Result<()> {
    if paths.len() > MAX_DURABLE_TOOL_EXECUTION_PATHS {
        anyhow::bail!("tool execution path labels exceed maximum count");
    }
    paths
        .iter()
        .try_for_each(|path| validate_relative_label(path))
}

fn validate_durable_execution_details(details: &serde_json::Value) -> anyhow::Result<()> {
    let Some(object) = details.as_object() else {
        if details.is_null() {
            return Ok(());
        }
        anyhow::bail!("durable tool execution details must be an object or null");
    };
    for key in object.keys() {
        if !DURABLE_TOOL_EXECUTION_DETAIL_KEYS.contains(&key.as_str()) {
            anyhow::bail!("durable tool execution details contain unsupported key {key}");
        }
    }
    for key in [
        "command_sha256",
        "details_sha256",
        "output_hash",
        "permission_plan_hash",
        "shell_sha256",
    ] {
        if let Some(value) = object.get(key) {
            let hash = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("durable tool execution {key} must be a digest"))?;
            validate_sha256("durable tool execution detail hash", hash)?;
        }
    }
    if let Some(value) = object.get("cwd_label") {
        validate_relative_label(
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("terminal cwd label must be text"))?,
        )?;
    }
    for key in ["approval_request_id", "log_ref", "shell_label", "task_id"] {
        if let Some(value) = object.get(key) {
            validate_durable_identity(
                "durable tool execution detail identity",
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("durable tool execution {key} must be text"))?,
            )?;
        }
    }
    if let Some(call) = object.get("call") {
        validate_durable_tool_call_context(call)?;
    }
    if crate::safe_persistence_json_value(details.clone()) != *details {
        anyhow::bail!("durable tool execution details contain unsafe text");
    }
    if json_value_contains_absolute_path(details) {
        anyhow::bail!("durable tool execution details contain an absolute path");
    }
    let bytes =
        serde_json::to_vec(details).context("failed to size durable tool execution details")?;
    if bytes.len() > MAX_DURABLE_TOOL_EXECUTION_DETAILS_BYTES {
        anyhow::bail!("durable tool execution details exceed maximum byte size");
    }
    Ok(())
}

fn validate_durable_tool_call_context(context: &serde_json::Value) -> anyhow::Result<()> {
    let object = context
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("durable tool call context must be an object"))?;
    for key in object.keys() {
        if ![
            "command_sha256",
            "path_sha256",
            "pattern_sha256",
            "subjects",
            "summary",
        ]
        .contains(&key.as_str())
        {
            anyhow::bail!("durable tool call context contains unsupported key {key}");
        }
    }
    for key in ["command_sha256", "path_sha256", "pattern_sha256"] {
        if let Some(value) = object.get(key) {
            validate_sha256(
                "durable tool call context hash",
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("tool call context {key} must be text"))?,
            )?;
        }
    }
    if let Some(subjects) = object.get("subjects") {
        let subjects = subjects
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("tool call context subjects must be an array"))?;
        if subjects.len() > 6 {
            anyhow::bail!("tool call context subjects exceed maximum count");
        }
        for subject in subjects {
            validate_bounded_safe_text(
                "tool call context subject",
                subject
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("tool call context subject must be text"))?,
                MAX_DURABLE_SUBJECT_LABEL_BYTES,
            )?;
        }
    }
    if let Some(summary) = object.get("summary") {
        validate_bounded_safe_text(
            "tool call context summary",
            summary
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("tool call context summary must be text"))?,
            MAX_DURABLE_PERMISSION_TEXT_BYTES,
        )?;
    }
    Ok(())
}

fn json_value_contains_absolute_path(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::String(value) => string_contains_absolute_path(value),
        serde_json::Value::Array(values) => values.iter().any(json_value_contains_absolute_path),
        serde_json::Value::Object(values) => values.values().any(json_value_contains_absolute_path),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            false
        }
    }
}

fn string_contains_absolute_path(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let token = token.trim_matches(|character: char| {
            matches!(character, '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';')
        });
        std::path::Path::new(token).is_absolute()
            || (token.as_bytes().get(1) == Some(&b':')
                && token
                    .as_bytes()
                    .get(2)
                    .is_some_and(|byte| matches!(byte, b'/' | b'\\')))
    })
}

fn validate_durable_execution_error_details(details: &serde_json::Value) -> anyhow::Result<()> {
    if details.is_null() {
        return Ok(());
    }
    let Some(object) = details.as_object() else {
        anyhow::bail!("durable tool execution error details must be null or a digest object");
    };
    if object.len() != 2
        || object.get("redacted") != Some(&serde_json::Value::Bool(true))
        || object
            .get("sha256")
            .and_then(serde_json::Value::as_str)
            .is_none_or(|hash| validate_sha256("tool execution error details hash", hash).is_err())
    {
        anyhow::bail!("durable tool execution error details are invalid");
    }
    Ok(())
}

pub(crate) const DURABLE_TOOL_EXECUTION_DETAIL_KEYS: &[&str] = &[
    "approval_request_id",
    "call",
    "cleanup",
    "command_sha256",
    "created_at_ms",
    "cwd_label",
    "details_sha256",
    "execution_binding",
    "execution_containment",
    "execution_mutation_profile",
    "generation",
    "log_ref",
    "output_hash",
    "output_limit_bytes",
    "output_termination_reason",
    "output_total_bytes",
    "output_truncated",
    "permission_plan_hash",
    "prepared_mutation",
    "readiness",
    "schema_version",
    "shell_label",
    "shell_sha256",
    "status",
    "status_detail",
    "task_id",
    "updated_at_ms",
];

/// Append-only audit entry for one outbound tool call summary.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct ToolEgressEntry {
    pub call_id: String,
    pub tool_name: String,
    pub destination: String,
    pub operation: String,
    pub subjects: Vec<ToolSubjectAudit>,
    pub payload: serde_json::Value,
    pub redacted: bool,
}

/// Append-only audit entry for one MCP elicitation decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct McpElicitationEntry {
    pub server_name: String,
    pub message_preview: String,
    pub message_hash: String,
    pub requested_schema_hash: String,
    pub requested_field_names: Vec<String>,
    pub required_field_names: Vec<String>,
    pub action: McpElicitationDecision,
    pub content_field_names: Vec<String>,
    pub content_redacted: bool,
}

impl McpElicitationEntry {
    pub fn new(
        server_name: impl Into<String>,
        message: &str,
        requested_schema: &serde_json::Value,
        action: McpElicitationDecision,
        content: Option<&serde_json::Value>,
    ) -> Self {
        Self {
            server_name: server_name.into(),
            message_preview: truncate_stable(message, 160),
            message_hash: stable_text_hash(message),
            requested_schema_hash: stable_json_hash(requested_schema),
            requested_field_names: json_object_keys(requested_schema.get("properties")),
            required_field_names: json_string_array(requested_schema.get("required")),
            action,
            content_field_names: content.map(json_top_level_keys).unwrap_or_default(),
            content_redacted: content.is_some(),
        }
    }
}

fn truncate_stable(content: &str, max_chars: usize) -> String {
    let normalized = content.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = normalized.chars().count();
    if char_count <= max_chars {
        return normalized;
    }
    let truncated = normalized.chars().take(max_chars).collect::<String>();
    format!("{truncated}...")
}

/// Stable MCP elicitation user decision persisted in the control log.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum McpElicitationDecision {
    Accepted,
    Declined,
    Cancelled,
}

/// Stable execution status for session audit records.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolExecutionStatus {
    Started,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

/// Durable subject snapshot for one permission or execution audit record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolSubjectAudit {
    pub kind: ToolSubjectKind,
    pub scope: ToolSubjectScope,
    pub identity_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_path_sha256: Option<String>,
}

impl From<&ToolSubject> for ToolSubjectAudit {
    fn from(subject: &ToolSubject) -> Self {
        let identity_value = tool_subject_identity_value(subject);
        Self {
            kind: subject.kind,
            scope: subject.scope,
            identity_sha256: crate::stable_event_hash(identity_value.as_bytes()),
            relative_label: tool_subject_relative_label(subject),
            canonical_path_sha256: subject.canonical_path.as_ref().map(|path| {
                crate::stable_event_hash(path.as_os_str().to_string_lossy().as_bytes())
            }),
        }
    }
}

impl ToolSubjectAudit {
    pub(crate) fn validate(&self) -> anyhow::Result<()> {
        validate_sha256("tool subject identity hash", &self.identity_sha256)?;
        if let Some(label) = &self.relative_label {
            validate_relative_label(label)?;
        }
        if let Some(hash) = &self.canonical_path_sha256 {
            validate_sha256("tool subject canonical path hash", hash)?;
        }
        Ok(())
    }
}

fn tool_subject_identity_value(subject: &ToolSubject) -> String {
    if subject.kind == ToolSubjectKind::McpTrustClass {
        subject.original.clone()
    } else if subject.scope == ToolSubjectScope::External {
        subject
            .canonical_path
            .as_ref()
            .map(|path| path.as_os_str().to_string_lossy().into_owned())
            .unwrap_or_else(|| subject.normalized.clone())
    } else {
        subject.normalized.clone()
    }
}

fn tool_subject_relative_label(subject: &ToolSubject) -> Option<String> {
    if subject.kind != ToolSubjectKind::Path || subject.scope != ToolSubjectScope::Workspace {
        return None;
    }
    let value = subject.normalized.trim();
    if validate_relative_label(value).is_err() {
        return None;
    }
    Some(value.to_owned())
}

fn validate_permission_audit_collections(
    subjects: &[ToolSubjectAudit],
    matches: &[CommandPermissionMatch],
    reasons: &[PermissionDecisionReason],
) -> anyhow::Result<()> {
    if subjects.len() > crate::MAX_TOOL_PERMISSION_SUBJECTS {
        anyhow::bail!("permission audit subjects exceed maximum count");
    }
    subjects.iter().try_for_each(ToolSubjectAudit::validate)?;
    if matches.len() > MAX_DURABLE_PERMISSION_MATCHES {
        anyhow::bail!("permission audit command matches exceed maximum count");
    }
    for matched in matches {
        for value in [&matched.pattern, &matched.command] {
            validate_sha256("permission audit command match hash", value)?;
        }
    }
    if reasons.len() > MAX_DURABLE_PERMISSION_REASONS {
        anyhow::bail!("permission audit decision reasons exceed maximum count");
    }
    for reason in reasons {
        validate_bounded_safe_text("permission audit decision reason code", &reason.code, 96)?;
        validate_bounded_safe_text(
            "permission audit decision reason detail",
            &reason.detail,
            MAX_DURABLE_PERMISSION_TEXT_BYTES,
        )?;
    }
    Ok(())
}

fn validate_approval_identity(identity: &ApprovalRequestIdentityV2) -> anyhow::Result<()> {
    for (label, value) in [
        ("approval session id", identity.session_id.as_str()),
        ("approval run id", identity.run_id.as_str()),
        ("approval call id", identity.call_id.as_str()),
        ("approval request id", identity.approval_request_id.as_str()),
    ] {
        validate_durable_identity(label, value)?;
    }
    validate_sha256("approval plan hash", &identity.plan_hash)?;
    validate_bounded_safe_text(
        "approval policy version",
        &identity.policy_version,
        MAX_DURABLE_PERMISSION_TEXT_BYTES,
    )?;
    validate_sha256(
        "approval execution binding hash",
        &identity.execution_binding_hash,
    )
}

fn validate_durable_identity(label: &str, value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 256
        || crate::safe_persistence_text(value) != value
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
    {
        anyhow::bail!("{label} is not a bounded safe identity");
    }
    Ok(())
}

fn validate_sha256(label: &str, value: &str) -> anyhow::Result<()> {
    let digest = value
        .strip_prefix(crate::event::RECORD_CHECKSUM_PREFIX)
        .or_else(|| value.strip_prefix("sha256:"))
        .unwrap_or(value);
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("{label} is not a sha256 digest");
    }
    Ok(())
}

fn validate_bounded_safe_text(label: &str, value: &str, max_bytes: usize) -> anyhow::Result<()> {
    if value.len() > max_bytes || crate::safe_persistence_text(value) != value {
        anyhow::bail!("{label} is not bounded safe text");
    }
    Ok(())
}

fn validate_relative_label(value: &str) -> anyhow::Result<()> {
    let path = std::path::Path::new(value);
    if value.is_empty()
        || value.len() > MAX_DURABLE_SUBJECT_LABEL_BYTES
        || path.is_absolute()
        || value.contains('\\')
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || crate::safe_persistence_text(value) != value
    {
        anyhow::bail!("durable relative label is invalid");
    }
    Ok(())
}

fn validate_serialized_tool_control_size<T: Serialize>(value: &T) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec(value).context("failed to size durable tool control entry")?;
    if bytes.len() > MAX_DURABLE_TOOL_CONTROL_BYTES {
        anyhow::bail!(
            "durable tool control entry exceeds maximum of {} bytes",
            MAX_DURABLE_TOOL_CONTROL_BYTES
        );
    }
    Ok(())
}
