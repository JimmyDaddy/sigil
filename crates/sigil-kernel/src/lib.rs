pub mod agent;
pub mod agent_thread;
pub mod approval;
pub mod changeset;
pub mod config;
pub mod conversation_queue;
pub mod event;
pub mod memory;
pub mod mutation;
pub mod permission;
pub mod plan;
pub mod plugin;
pub mod provider;
pub mod secret;
pub mod session;
pub mod skill;
pub mod task;
pub mod task_orchestrator;
pub mod terminal_task;
pub mod time;
pub mod tool;
pub mod verification;

pub use agent::{
    Agent, AgentDelegationRequirement, AgentRunInput, AgentRunOptions, AgentRunOutcome,
    AgentRunOutput, AgentRunResult, AgentRunTerminalReason, AgentToolDelegate,
};
pub use agent_thread::{
    AgentApprovalRouteEntry, AgentArtifactRef, AgentElicitationRouteEntry, AgentFinalAnswerRef,
    AgentInvocationMode, AgentInvocationPolicy, AgentInvocationRequest, AgentInvocationSource,
    AgentMergeSafePointEntry, AgentPermissionPolicy, AgentProfile, AgentProfileCapturedEntry,
    AgentProfileId, AgentProfileKind, AgentProfilePolicyEntry, AgentProfilePolicyProjection,
    AgentProfileSnapshot, AgentProfileSnapshotId, AgentProfileSource, AgentProfileTrustEntry,
    AgentProfileTrustProjection, AgentResultContinuationEntry, AgentResultContinuationProjection,
    AgentResultContinuationStatus, AgentResultPolicy, AgentRouteClosedEntry, AgentRouteId,
    AgentRouteStatus, AgentRunAttemptId, AgentRunAttemptProjection, AgentRunAttemptStartedEntry,
    AgentRunContextSnapshot, AgentRunHeartbeatEntry, AgentRunInterruptedEntry,
    AgentThreadClosedEntry, AgentThreadDisplayNameEntry, AgentThreadId,
    AgentThreadMessageRoutedEntry, AgentThreadProjection, AgentThreadResult,
    AgentThreadResultRecordedEntry, AgentThreadStartedEntry, AgentThreadStateProjection,
    AgentThreadStatus, AgentThreadStatusChangedEntry, AgentThreadTerminalStatus, AgentTrustState,
    AgentUsageSummary, WorkspaceRootSnapshot, closed_agent_routes, interrupted_agent_attempts,
};
pub use approval::{ApprovalHandler, AutoApproveHandler, ToolApproval};
pub use changeset::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetProjection, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk,
    ChangeSetState, ChangeSetValidation, ChangeSetValidationKind, ChangeSetValidationStatus,
};
pub use config::{
    AgentConfig, AppearanceConfig, CodeIntelStartup, CodeIntelligenceConfig,
    CodeIntelligenceDiscoveryConfig, CompactionConfig, CompactionThresholdStatus,
    LanguageServerConfig, McpServerConfig, McpServerPinnedIdentity, McpServerStartup,
    McpServerTrustPolicy, McpTrustClass, MemoryConfig, RoleModelConfig, RootConfig, SessionConfig,
    SkillConfig, StorageConfig, StorageRoot, SyntaxThemeId, TaskConfig, TaskMode,
    ThemeColorOverrides, ThemeId, ToolAllowlistConfig, UsageCostCurrency, WorkspaceConfig,
    default_user_config_dir, default_user_config_path, preferred_config_path,
    resolve_workspace_root,
};
pub use conversation_queue::{
    ConversationInputEditedEntry, ConversationInputKind, ConversationInputQueueControlAction,
    ConversationInputQueueControlEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputReorderedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, ConversationQueueItemProjection, ConversationQueueProjection,
};
pub use event::{
    ALL_DURABLE_EVENT_TYPES, DomainEvent, DomainPayload, DurableDomainEvent, DurableEventType,
    EventClass, EventHandler, EventId, EventSyncClass, LegacyEvent, MAX_EVENT_BYTES,
    MAX_PAYLOAD_DEPTH, NoopEventHandler, PUBLIC_RUN_EVENT_SCHEMA_VERSION, ProjectionApplyDecision,
    ProjectionCursor, PublicAssistantMessage, PublicControlEvent, PublicRunEvent,
    PublicRunEventKind, RECORD_CHECKSUM_PREFIX, ReducerDisposition, RunEvent,
    STORED_EVENT_SCHEMA_VERSION, SessionId, StoredEvent, StoredEventDecode, decode_stored_event,
    is_transient_run_event, projection_apply_decision, projection_apply_decision_for_record,
    reducer_disposition, stable_event_hash, stable_event_uuid,
};
pub use memory::{MemoryLoadReport, inspect_memory_documents};
pub use mutation::{
    CommittedFileMutation, MutationBatchId, MutationBatchStatus, MutationCommitted,
    MutationCoordinator, MutationEventRecorder, MutationObservedState, MutationPrepared,
    MutationReconciled, MutationResolution, MutationSubject, MutationSyncClass, OperationId,
    PreparedFileMutation, SnapshotCoverage, WorkspaceMutationDetected,
    WorkspaceMutationDetectionReason, WorkspaceMutationScan, bytes_hash, delete_file_with_mutation,
    delete_file_with_mutation_in_batch, file_content_hash, write_file_with_mutation,
    write_file_with_mutation_in_batch,
};
pub use permission::{
    ApprovalMode, EffectivePermissionPolicyCap, ExternalDirectoryConfig, ExternalDirectoryRule,
    InteractionMode, PathTrustZone, PermissionAccessConfig, PermissionConfig,
    PermissionConfirmation, PermissionDecision, PermissionEvaluationContext, PermissionPolicy,
    PermissionPreset, PermissionRisk, PermissionRule, ToolOperation, apply_risk_overlay,
    classify_path_trust_zone, derive_permission_risk, infer_tool_operation,
};
pub use plan::{
    PLAN_HASH_PREFIX, PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalProjection,
    PlanApprovalScope, PlanApprovedEntry, plan_text_hash, plan_workspace_paths,
};
pub use plugin::{
    PluginAgentRef, PluginCapability, PluginHookRef, PluginManifest, PluginManifestSnapshot,
    PluginSkillRef, PluginStateProjection, PluginTrustDecision, PluginTrustEntry,
    validate_plugin_id,
};
pub use provider::{
    BackgroundTaskHandle, BackgroundTaskStatus, CompletionRequest, MessageRole, ModelMessage,
    PrefixSnapshot, Provider, ProviderCapabilities, ProviderChunk, ProviderContinuationState,
    ReasoningArtifact, ReasoningEffort, ReasoningStreamSupport, ResponseHandle, SessionStats,
    ToolCall, ToolCallCompletionIdPolicy, ToolCallStreamAccumulator, UsageStats,
};
pub use secret::{REDACTED_SECRET, SecretRedactor};
pub use session::{
    CompactionPreview, CompactionRecord, ControlEntry, DomainEventRecord, JsonlSessionStore,
    McpElicitationDecision, McpElicitationEntry, MemorySnapshot, Session, SessionLogEntry,
    SessionStreamRecord, ToolApprovalAuditAction, ToolApprovalEntry, ToolApprovalUserDecision,
    ToolEgressEntry, ToolExecutionEntry, ToolExecutionStatus, ToolSubjectAudit,
    latest_compaction_record, session_stats_from_entries,
};
pub use skill::{
    SkillDescriptor, SkillIndexSnapshot, SkillLoadEntry, SkillLoadState, SkillRunMode, SkillSource,
    SkillStateProjection, SkillTrustState,
};
pub use task::{
    AgentRole, SessionRef, TASK_AGENT_DISPLAY_NAME_MAX_CHARS, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskChildSessionStatus, TaskId,
    TaskPlanEntry, TaskPlanProjection, TaskPlanStatus, TaskPlanUpdateContext, TaskRouteId,
    TaskRouteStatus, TaskRunEntry, TaskRunProjection, TaskRunStatus, TaskStateProjection,
    TaskStepEntry, TaskStepId, TaskStepProjection, TaskStepSpec, TaskStepStatus,
    TaskSubagentApprovalRouteEntry, TaskSubagentElicitationRouteEntry, child_session_ref,
    normalize_task_agent_display_name, task_plan_update_entry, task_plan_update_result_content,
    task_plan_update_tool_spec,
};
pub use task_orchestrator::{
    LegacyTaskChildSessionRunner, SequentialTaskOrchestrator, SequentialTaskRequest,
    SequentialTaskRunOutput, SequentialTaskStepOutput, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest, TaskChildSessionRunner,
};
pub use terminal_task::{
    TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId, TerminalTaskProjection,
    TerminalTaskStatus, TerminalTaskSummary,
};
pub use time::saturating_elapsed;
pub use tool::{
    ScopedToolRegistry, Tool, ToolAccess, ToolCategory, ToolContext, ToolDiffBudget, ToolDiffStats,
    ToolEgressAudit, ToolError, ToolErrorKind, ToolPreview, ToolPreviewCapability, ToolPreviewFile,
    ToolPreviewFileSnapshot, ToolPreviewSnapshot, ToolRegistry, ToolRegistryScope, ToolResult,
    ToolResultMeta, ToolResultStatus, ToolResultSummary, ToolSpec, ToolSubject, ToolSubjectKind,
    ToolSubjectScope,
};
pub use verification::{
    ArtifactId, CandidateCheck, ChangesetId, CheckCommand, CheckDiscoverySource, CheckPromotion,
    CheckSpec, CheckSpecId, CheckSpecRecordedEntry, ChildVerificationReceiptLinked,
    CompletionCriteria, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DiscoveredCheck,
    EnvironmentFingerprint, EvidenceReceipt, EvidenceScope, FileType, ReadinessEvaluatedEntry,
    ReadinessEvaluation, ReadinessInput, ReadinessProjectionMode, ReadinessReason, ReceiptId,
    ReceiptStatus, RedactionState, RequiredAction, RunStatus, SandboxDecisionId,
    SandboxProfileHash, SandboxProfileRequirement, SnapshotEntryState, ToolCallId, ToolEffect,
    TrustedCheckSpec, VerificationBinding, VerificationCheckConfig, VerificationCheckRunRequest,
    VerificationConfig, VerificationPolicy, VerificationPolicyChangedEntry, VerificationReceipt,
    VerificationRecordedEntry, VerificationScope, VerificationScopeHash, VerificationSkipDecision,
    VerificationStaleCause, VerificationStaleReason, VerificationStateProjection,
    VerificationVerdict, VisibleCompletionState, WorkspaceId, WorkspaceKnowledge,
    WorkspaceMutationEvidence, WorkspaceRevision, WorkspaceSnapshotBuild, WorkspaceSnapshotEntry,
    WorkspaceSnapshotId, WorkspaceSnapshotManifestV1, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    WorkspaceTrustRequirement, WorkspaceTrustSnapshotId, build_workspace_snapshot,
    build_workspace_snapshot_for_event, check_specs_from_user_config, default_scope_excludes,
    discover_candidate_checks, discover_candidate_checks_with_user_config, evaluate_readiness,
    run_verification_check, stable_workspace_id,
};
