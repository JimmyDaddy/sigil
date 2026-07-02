use super::*;

/// Append-only session log entry stored in the durable JSONL session file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // Keep the durable JSONL shape unboxed across control variants.
#[serde(rename_all = "snake_case")]
pub enum SessionLogEntry {
    #[serde(alias = "User")]
    User(ModelMessage),
    #[serde(alias = "Assistant")]
    Assistant(ModelMessage),
    #[serde(alias = "ToolResult")]
    ToolResult(ModelMessage),
    #[serde(alias = "Control")]
    Control(ControlEntry),
}

/// One physical record in a mixed legacy/v2 session stream.
#[derive(Debug, Clone)]
pub enum SessionStreamRecord {
    Legacy {
        event: LegacyEvent,
        entry: Box<SessionLogEntry>,
    },
    Stored(StoredEvent),
}

impl SessionStreamRecord {
    pub fn stream_sequence(&self) -> u64 {
        match self {
            Self::Legacy { event, .. } => event.stream_sequence,
            Self::Stored(event) => event.stream_sequence,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::Legacy { event, .. } => &event.session_id,
            Self::Stored(event) => &event.session_id,
        }
    }

    pub fn event_id(&self) -> &str {
        match self {
            Self::Legacy { event, .. } => &event.event_id,
            Self::Stored(event) => &event.event_id,
        }
    }

    pub fn record_checksum(&self) -> &str {
        match self {
            Self::Legacy { event, .. } => &event.raw_line_hash,
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
            Self::Legacy { event, .. } => Some(DomainEvent::Legacy(event.clone())),
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

    pub fn typed_domain_event_record(&self) -> Result<Option<TypedDomainEventRecord>> {
        let Self::Stored(event) = self else {
            return Ok(None);
        };
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

pub const SESSION_ENTRY_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_THREAD_STATE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_PROFILE_TRUST_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_PROFILE_POLICY_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const AGENT_RESULT_CONTINUATION_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const CHANGESET_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const CONVERSATION_QUEUE_PROJECTION_SCHEMA_VERSION: u16 = 1;
pub const PLAN_APPROVAL_PROJECTION_SCHEMA_VERSION: u16 = 1;
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

/// Stable compaction metadata persisted in the append-only control plane.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct CompactionRecord {
    pub summary: String,
    pub compacted_message_count: usize,
    pub retained_tail_message_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_memory: Option<crate::TaskMemoryV1>,
}

/// Deterministic preview of what one manual compaction would fold and project.
#[derive(Debug, Clone)]
pub struct CompactionPreview {
    pub record: CompactionRecord,
    pub folded_messages: Vec<ModelMessage>,
    pub projected_messages: Vec<ModelMessage>,
}

/// Stable memory payload captured for a specific memory fingerprint.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct MemorySnapshot {
    pub messages: Vec<ModelMessage>,
    pub report: MemoryLoadReport,
}

/// Audit entry recorded when Context V0 candidates are recalled but cannot be rendered safely.
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
    #[serde(alias = "SessionIdentity")]
    SessionIdentity {
        provider_name: String,
        model_name: String,
    },
    #[serde(alias = "ContinuationStateSaved")]
    ContinuationStateSaved(ProviderContinuationState),
    #[serde(alias = "ResponseHandleTracked")]
    ResponseHandleTracked(crate::provider::ResponseHandle),
    #[serde(alias = "BackgroundTaskTracked")]
    BackgroundTaskTracked(crate::provider::BackgroundTaskHandle),
    #[serde(alias = "PrefixSnapshotCaptured")]
    PrefixSnapshotCaptured(PrefixSnapshot),
    #[serde(alias = "MemorySnapshotCaptured")]
    MemorySnapshotCaptured(MemorySnapshot),
    #[serde(alias = "ContextAssemblySkipped")]
    ContextAssemblySkipped(ContextAssemblySkippedEntry),
    #[serde(alias = "UsageSnapshot")]
    UsageSnapshot(UsageStats),
    #[serde(alias = "ToolApproval")]
    ToolApproval(ToolApprovalEntry),
    #[serde(alias = "ToolApprovalSessionGrant")]
    ToolApprovalSessionGrant(ToolApprovalSessionGrantEntry),
    #[serde(alias = "ToolExecution")]
    ToolExecution(Box<ToolExecutionEntry>),
    #[serde(alias = "ToolEgress")]
    ToolEgress(Box<ToolEgressEntry>),
    #[serde(alias = "McpElicitation")]
    McpElicitation(Box<McpElicitationEntry>),
    #[serde(alias = "ToolPreviewCaptured")]
    ToolPreviewCaptured(ToolPreviewSnapshot),
    #[serde(alias = "SkillIndexCaptured")]
    SkillIndexCaptured(SkillIndexSnapshot),
    #[serde(alias = "SkillLoaded")]
    SkillLoaded(SkillLoadEntry),
    #[serde(alias = "PluginManifestCaptured")]
    PluginManifestCaptured(PluginManifestSnapshot),
    #[serde(alias = "PluginTrustDecision")]
    PluginTrustDecision(PluginTrustEntry),
    #[serde(alias = "PluginHookExecutionStarted")]
    PluginHookExecutionStarted(PluginHookExecutionStartedEntry),
    #[serde(alias = "PluginHookExecutionFinished")]
    PluginHookExecutionFinished(PluginHookExecutionFinishedEntry),
    #[serde(alias = "ChangeSetProposed")]
    ChangeSetProposed(ChangeSet),
    #[serde(alias = "ChangeSetApplied")]
    ChangeSetApplied(ChangeSetResult),
    #[serde(alias = "TerminalTask")]
    TerminalTask(TerminalTaskEntry),
    #[serde(alias = "CompactionApplied")]
    CompactionApplied(CompactionRecord),
    #[serde(alias = "PlanApproved")]
    PlanApproved(PlanApprovedEntry),
    #[serde(alias = "PlanDraftCreated")]
    PlanDraftCreated(PlanDraftCreatedEntry),
    #[serde(alias = "PlanDecisionRecorded")]
    PlanDecisionRecorded(PlanDecisionRecordedEntry),
    #[serde(alias = "PlanPermissionGranted")]
    PlanPermissionGranted(PlanPermissionGrantedEntry),
    #[serde(alias = "TaskCreatedFromPlan")]
    TaskCreatedFromPlan(TaskCreatedFromPlanEntry),
    #[serde(alias = "TaskRun")]
    TaskRun(TaskRunEntry),
    #[serde(alias = "TaskPlan")]
    TaskPlan(TaskPlanEntry),
    #[serde(alias = "TaskStep")]
    TaskStep(TaskStepEntry),
    #[serde(alias = "TaskChildSession")]
    TaskChildSession(TaskChildSessionEntry),
    #[serde(alias = "TaskChildSessionDisplayName")]
    TaskChildSessionDisplayName(TaskChildSessionDisplayNameEntry),
    #[serde(alias = "TaskSubagentApprovalRoute")]
    TaskSubagentApprovalRoute(TaskSubagentApprovalRouteEntry),
    #[serde(alias = "TaskSubagentElicitationRoute")]
    TaskSubagentElicitationRoute(TaskSubagentElicitationRouteEntry),
    #[serde(alias = "JobIntentRecorded")]
    JobIntentRecorded(crate::resume::JobIntentEntry),
    #[serde(alias = "StepLeaseRecorded")]
    StepLeaseRecorded(crate::resume::StepLeaseEntry),
    #[serde(alias = "StepLeaseHeartbeatRecorded")]
    StepLeaseHeartbeatRecorded(crate::resume::StepLeaseHeartbeatEntry),
    #[serde(alias = "CheckSpecRecorded")]
    CheckSpecRecorded(CheckSpecRecordedEntry),
    #[serde(alias = "VerificationPolicyChanged")]
    VerificationPolicyChanged(VerificationPolicyChangedEntry),
    #[serde(alias = "VerificationCheckRun")]
    VerificationCheckRun(VerificationCheckRunEntry),
    #[serde(alias = "VerificationRecorded")]
    VerificationRecorded(VerificationRecordedEntry),
    #[serde(alias = "ReadinessEvaluated")]
    ReadinessEvaluated(ReadinessEvaluatedEntry),
    #[serde(alias = "ChildVerificationReceiptLinked")]
    ChildVerificationReceiptLinked(ChildVerificationReceiptLinked),
    #[serde(alias = "WorkspaceTrustDecision")]
    WorkspaceTrustDecision(WorkspaceTrustDecisionEntry),
    #[serde(alias = "WriteLeaseAcquired")]
    WriteLeaseAcquired(WriteLeaseAcquired),
    #[serde(alias = "WriteLeaseReleased")]
    WriteLeaseReleased(WriteLeaseReleased),
    #[serde(alias = "IsolatedWorkspaceCreated")]
    IsolatedWorkspaceCreated(IsolatedWorkspaceCreated),
    #[serde(alias = "IsolatedChangeSetProduced")]
    IsolatedChangeSetProduced(IsolatedChangeSetProduced),
    #[serde(alias = "MergeReviewRequested")]
    MergeReviewRequested(MergeReviewRequested),
    #[serde(alias = "MergeReviewResolved")]
    MergeReviewResolved(MergeReviewResolved),
    #[serde(alias = "AgentProfileCaptured")]
    AgentProfileCaptured(AgentProfileCapturedEntry),
    #[serde(alias = "AgentProfileTrustDecision")]
    AgentProfileTrustDecision(AgentProfileTrustEntry),
    #[serde(alias = "AgentProfilePolicyDecision")]
    AgentProfilePolicyDecision(AgentProfilePolicyEntry),
    #[serde(alias = "AgentThreadStarted")]
    AgentThreadStarted(AgentThreadStartedEntry),
    #[serde(alias = "AgentThreadStatusChanged")]
    AgentThreadStatusChanged(AgentThreadStatusChangedEntry),
    #[serde(alias = "AgentThreadMessageRouted")]
    AgentThreadMessageRouted(AgentThreadMessageRoutedEntry),
    #[serde(alias = "AgentMailboxMessage")]
    AgentMailboxMessage(AgentMailboxMessageEntry),
    #[serde(alias = "AgentThreadResultRecorded")]
    AgentThreadResultRecorded(AgentThreadResultRecordedEntry),
    #[serde(alias = "AgentThreadResultDelivered")]
    AgentThreadResultDelivered(AgentThreadResultDeliveredEntry),
    #[serde(alias = "AgentResultContinuation")]
    AgentResultContinuation(AgentResultContinuationEntry),
    #[serde(alias = "AgentThreadDisplayName")]
    AgentThreadDisplayName(AgentThreadDisplayNameEntry),
    #[serde(alias = "AgentApprovalRoute")]
    AgentApprovalRoute(AgentApprovalRouteEntry),
    #[serde(alias = "AgentElicitationRoute")]
    AgentElicitationRoute(AgentElicitationRouteEntry),
    #[serde(alias = "AgentRunAttemptStarted")]
    AgentRunAttemptStarted(AgentRunAttemptStartedEntry),
    #[serde(alias = "AgentRunHeartbeat")]
    AgentRunHeartbeat(AgentRunHeartbeatEntry),
    #[serde(alias = "AgentRunInterrupted")]
    AgentRunInterrupted(AgentRunInterruptedEntry),
    #[serde(alias = "AgentRouteClosed")]
    AgentRouteClosed(AgentRouteClosedEntry),
    #[serde(alias = "AgentMergeSafePoint")]
    AgentMergeSafePoint(AgentMergeSafePointEntry),
    #[serde(alias = "AgentThreadClosed")]
    AgentThreadClosed(AgentThreadClosedEntry),
    #[serde(alias = "ConversationInputQueued")]
    ConversationInputQueued(ConversationInputQueuedEntry),
    #[serde(alias = "ConversationInputQueueControl")]
    ConversationInputQueueControl(ConversationInputQueueControlEntry),
    #[serde(alias = "ConversationInputEdited")]
    ConversationInputEdited(ConversationInputEditedEntry),
    #[serde(alias = "ConversationInputReordered")]
    ConversationInputReordered(ConversationInputReorderedEntry),
    #[serde(alias = "ConversationInputStatusChanged")]
    ConversationInputStatusChanged(ConversationInputStatusEntry),
    #[serde(alias = "Note")]
    Note {
        kind: String,
        data: serde_json::Value,
    },
}

/// Append-only audit entry for permission policy evaluation and interactive approval decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ToolApprovalEntry {
    pub action: ToolApprovalAuditAction,
    pub call_id: String,
    pub tool_name: String,
    pub access: ToolAccess,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<ToolOperation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<PermissionRisk>,
    pub subjects: Vec<ToolSubjectAudit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_zones: Vec<PathTrustZone>,
    pub policy_decision: ApprovalMode,
    pub external_directory_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<PermissionConfirmation>,
    #[serde(default)]
    pub snapshot_required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_source: Option<ToolApprovalAllowSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant_call_id: Option<String>,
    pub user_decision: Option<ToolApprovalUserDecision>,
    pub reason: Option<String>,
    pub preview_hash: Option<String>,
}

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
    PolicyEvaluated,
    Requested,
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

/// Append-only session-local approval grant created from an interactive tool approval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ToolApprovalSessionGrantEntry {
    pub call_id: String,
    pub tool_name: String,
    pub access: ToolAccess,
    pub operation: ToolOperation,
    pub risk: PermissionRisk,
    pub subjects: Vec<ToolSubjectAudit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subject_zones: Vec<PathTrustZone>,
    pub expires: ToolApprovalSessionGrantExpiry,
    pub granted_at_ms: u64,
}

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
    pub original: String,
    pub normalized: String,
    pub canonical_path: Option<String>,
    pub scope: ToolSubjectScope,
}

impl From<&ToolSubject> for ToolSubjectAudit {
    fn from(subject: &ToolSubject) -> Self {
        Self {
            kind: subject.kind,
            original: subject.original.clone(),
            normalized: subject.normalized.clone(),
            canonical_path: subject
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string()),
            scope: subject.scope,
        }
    }
}
