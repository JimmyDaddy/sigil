pub mod agent;
pub mod agent_thread;
pub mod approval;
pub mod changeset;
pub mod config;
pub mod context_engine;
pub mod conversation_queue;
pub mod eval;
pub mod event;
pub mod execution_backend;
pub mod memory;
pub mod mutation;
pub mod permission;
pub mod plan;
pub mod plugin;
pub mod projection;
pub mod provider;
pub mod provider_timeout;
pub mod resume;
pub mod secret;
pub mod session;
pub mod skill;
pub mod task;
pub mod task_memory;
pub mod task_orchestrator;
pub mod terminal_task;
pub mod time;
pub mod tool;
pub mod verification;
pub mod write_isolation;

pub use agent::{
    Agent, AgentDelegationRequirement, AgentRunInput, AgentRunOptions, AgentRunOutcome,
    AgentRunOutput, AgentRunResult, AgentRunTerminalReason, AgentToolDelegate, FinalAnswerContext,
    projected_agent_run_readiness,
};
pub use agent_thread::{
    AgentApprovalRouteEntry, AgentArtifactRef, AgentElicitationRouteEntry, AgentFinalAnswerRef,
    AgentGraphSummary, AgentInvocationMode, AgentInvocationPolicy, AgentInvocationRequest,
    AgentInvocationSource, AgentMailboxMessageEntry, AgentMailboxStatus, AgentMergeSafePointEntry,
    AgentPermissionPolicy, AgentProfile, AgentProfileCapturedEntry, AgentProfileId,
    AgentProfileKind, AgentProfilePolicyEntry, AgentProfilePolicyProjection, AgentProfileSnapshot,
    AgentProfileSnapshotId, AgentProfileSource, AgentProfileTrustEntry,
    AgentProfileTrustProjection, AgentResultContinuationEntry, AgentResultContinuationProjection,
    AgentResultContinuationStatus, AgentResultPolicy, AgentRouteClosedEntry, AgentRouteId,
    AgentRouteStatus, AgentRunAttemptId, AgentRunAttemptProjection, AgentRunAttemptStartedEntry,
    AgentRunContextSnapshot, AgentRunHeartbeatEntry, AgentRunInterruptedEntry,
    AgentThreadClosedEntry, AgentThreadDisplayNameEntry, AgentThreadId,
    AgentThreadMessageRoutedEntry, AgentThreadProjection, AgentThreadResult,
    AgentThreadResultDeliveredEntry, AgentThreadResultRecordedEntry, AgentThreadStartedEntry,
    AgentThreadStateProjection, AgentThreadStatus, AgentThreadStatusChangedEntry,
    AgentThreadTerminalStatus, AgentTrustState, AgentUsageSummary, WorkspaceRootSnapshot,
    closed_agent_routes, interrupted_agent_attempts, interrupted_agent_mailbox_messages,
};
pub use approval::{ApprovalHandler, AutoApproveHandler, ToolApproval};
pub use changeset::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetProjection, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk,
    ChangeSetState, ChangeSetValidation, ChangeSetValidationKind, ChangeSetValidationStatus,
};
pub use config::{
    AgentConfig, AppearanceConfig, CodeIntelStartup, CodeIntelligenceConfig, CompactionConfig,
    CompactionThresholdStatus, DEFAULT_MUTATION_ARTIFACT_RETENTION_EXPIRE_OLDER_THAN_MS,
    DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_ARTIFACTS,
    DEFAULT_MUTATION_ARTIFACT_RETENTION_MAX_BYTES, LanguageServerConfig, McpServerConfig,
    McpServerPinnedIdentity, McpServerStartup, McpServerTrustPolicy, McpTrustClass, MemoryConfig,
    ModelRequestConfig, ModelRequestTimeouts, MutationArtifactRetentionConfig, RoleModelConfig,
    RootConfig, SIGIL_MODEL_REQUEST_TIMEOUT_SECS_ENV, SIGIL_MODEL_STREAM_IDLE_TIMEOUT_SECS_ENV,
    SIGIL_MODEL_STREAM_TOTAL_TIMEOUT_SECS_ENV, SessionConfig, SkillConfig, StorageConfig,
    StorageRoot, SyntaxThemeId, TaskConfig, TaskMode, TerminalKeyboardEnhancement,
    ThemeColorOverrides, ThemeId, ToolAllowlistConfig, UsageCostCurrency, WorkspaceConfig,
    default_user_config_dir, default_user_config_path, preferred_config_path,
    resolve_workspace_root,
};
pub use context_engine::{
    CONTEXT_QUALITY_EVIDENCE_SCHEMA_VERSION, CONTEXT_QUALITY_REPORT_SCHEMA_VERSION, ContextBodyRef,
    ContextDigestText, ContextDigestTextKind, ContextDigestV0, ContextDigestV0Builder,
    ContextEgressDecisionId, ContextInclusionReason, ContextItem, ContextItemId,
    ContextPackOptions, ContextPackPlacement, ContextPlacementMissingReason,
    ContextProvenanceRowV1, ContextQualityEvidencePack, ContextQualityFinding,
    ContextQualityFindingKind, ContextQualityItemEvidence, ContextQualityMatrixEntry,
    ContextQualityReportArtifacts, ContextQualityReportManifest, ContextRepoRevision,
    ContextScoreComponent, ContextScoreComponentKind, ContextScoreMissingReason,
    ContextSensitivity, ContextSource, ContextSourceRef, ContextTruncation, ContextTrustLevel,
    DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES, DEFAULT_SESSION_ARCHIVE_MAX_INDEX_BYTES,
    PackedContext, RuntimeContextCandidates, SessionArchive, SessionArchiveEntry,
    SessionArchiveEntryId, SessionArchiveSearchHit, UNKNOWN_CONTEXT_REPO_REVISION,
    build_context_quality_evidence_pack, context_provenance_row_v1, estimate_context_token_cost,
    pack_context_items, validate_context_render_snippet, write_context_quality_evidence_artifacts,
};
pub use conversation_queue::{
    ConversationInputEditedEntry, ConversationInputKind, ConversationInputQueueControlAction,
    ConversationInputQueueControlEntry, ConversationInputQueueId, ConversationInputQueuedEntry,
    ConversationInputReorderedEntry, ConversationInputStatus, ConversationInputStatusEntry,
    ConversationInputTarget, ConversationQueueItemProjection, ConversationQueueProjection,
};
pub use eval::{
    EvalCase, EvalCaseId, EvalCaseProvenance, EvalCaseRunner, EvalCaseRunnerOptions,
    EvalEvidenceId, EvalEvidenceKind, EvalEvidenceRef, EvalFailure, EvalFailureKind,
    EvalFakeToolAction, EvalFakeToolRegistry, EvalFixtureId, EvalOutcomeKind, EvalProviderScript,
    EvalProviderStep, EvalRepoCheckPromotion, EvalReportArtifact, EvalReportArtifacts,
    EvalReportManifest, EvalReportMatrixEntry, EvalReportRecord, EvalRequiredAction,
    EvalRequiredActionKind, EvalResult, EvalRunId, EvalRunMetadata, EvalStepId, EvalToolCallId,
    EvalToolCallStatus, EvalToolCallSummary, EvalWorkspaceFixture, write_eval_report_artifacts,
};
pub use event::{
    ALL_DURABLE_EVENT_TYPES, DomainEvent, DomainPayload, DurableDomainEvent, DurableEventType,
    EventClass, EventHandler, EventId, EventSyncClass, MAX_EVENT_BYTES, MAX_PAYLOAD_DEPTH,
    NoopEventHandler, PUBLIC_RUN_EVENT_SCHEMA_VERSION, ProjectionApplyDecision, ProjectionCursor,
    PublicAssistantMessage, PublicControlEvent, PublicRunEvent, PublicRunEventKind,
    RECORD_CHECKSUM_PREFIX, ReducerDisposition, RunEvent, STORED_EVENT_SCHEMA_VERSION, SessionId,
    StoredEvent, StoredEventDecode, TypedDomainEvent, TypedStoredEventDecode, decode_stored_event,
    decode_typed_stored_event, is_transient_run_event, projection_apply_decision,
    projection_apply_decision_for_record, reducer_disposition, stable_event_hash,
    stable_event_uuid,
};
pub use execution_backend::{
    ExecutionBackend, ExecutionBackendCapabilities, ExecutionBackendKind,
    ExecutionBackendSelectionDecision, ExecutionBackendSelectionDiagnostic, ExecutionCapability,
    ExecutionCapabilityRequirements, ExecutionCleanupReceipt, ExecutionCleanupStatus,
    ExecutionConfig, ExecutionCoverageLabel, ExecutionCoverageSummary, ExecutionFuture,
    ExecutionIsolationPolicy, ExecutionNetworkPolicy, ExecutionNetworkReceipt, ExecutionReceipt,
    ExecutionRequest, ExecutionResourceLimitKind, ExecutionResourceLimitReceipt,
    ExecutionResourceReceipt, ExecutionSandboxFallback, ExecutionSandboxProfile,
    ExecutionSandboxProfileSpec, ExecutionSandboxStrategyConfig, ExecutionStrategyConfig,
    ExecutionStrategyMode, ExecutionTimeoutSource,
};
pub use memory::{MemoryLoadReport, inspect_memory_documents};
pub use mutation::{
    CheckpointRestored, CommittedDirectoryMutation, CommittedFileMutation,
    ExecutionMutationProfile, MutationArtifactCleanupRequested, MutationArtifactCleanupTarget,
    MutationArtifactId, MutationArtifactInventoryItem, MutationArtifactLifecycleRecorded,
    MutationArtifactLifecycleStatus, MutationArtifactRetentionPolicy,
    MutationArtifactRetentionReport, MutationBatchId, MutationBatchStatus, MutationCommitted,
    MutationCoordinator, MutationEventRecorder, MutationObservedState, MutationPrepared,
    MutationReconciled, MutationResolution, MutationSubject, MutationSyncClass, OperationId,
    PreparedDirectoryMutation, PreparedFileMutation, RestoredFileMutation, SnapshotCoverage,
    WorkspaceMutationDetected, WorkspaceMutationDetectionReason, WorkspaceMutationScan, bytes_hash,
    create_directory_with_mutation, delete_directory_with_mutation, delete_file_with_mutation,
    delete_file_with_mutation_in_batch, file_content_hash,
    restore_file_from_snapshot_with_mutation, write_file_with_mutation,
    write_file_with_mutation_in_batch,
};
pub use permission::{
    ApprovalMode, EffectivePermissionPolicyCap, ExternalDirectoryConfig, ExternalDirectoryRule,
    InteractionMode, PathTrustZone, PermissionConfig, PermissionConfirmation, PermissionDecision,
    PermissionEvaluationContext, PermissionMode, PermissionPolicy, PermissionRisk, PermissionRule,
    ToolOperation, apply_risk_overlay, classify_path_trust_zone, derive_permission_risk,
    infer_tool_operation, tool_approval_session_grant_available,
    tool_approval_session_grant_available_for_parts,
};
pub use plan::{
    PLAN_HASH_PREFIX, PlanApprovalExpiry, PlanApprovalPermission, PlanApprovalProjection,
    PlanApprovalScope, PlanApprovedEntry, PlanArtifactProjection, PlanDecision, PlanDecisionActor,
    PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanDraftStep, PlanId,
    PlanPermissionGrantedEntry, PlanSourceRef, PlanSuggestedCheck, PlanTaskStartMode,
    PlanToTaskStepMapping, TaskCreatedFromPlanEntry, plan_draft_created_entry,
    plan_task_input_from_draft, plan_text_hash, plan_workspace_paths,
};
pub use plugin::{
    DEFAULT_PLUGIN_HOOK_OUTPUT_LIMIT_BYTES, DEFAULT_PLUGIN_HOOK_TIMEOUT_MS,
    MAX_PLUGIN_HOOK_ARTIFACT_REFS, MAX_PLUGIN_HOOK_OUTPUT_LIMIT_BYTES, MAX_PLUGIN_HOOK_TIMEOUT_MS,
    PLUGIN_MANIFEST_DIGEST_PREFIX, PluginAgentRef, PluginCapability, PluginCapabilityPolicy,
    PluginHookContextItems, PluginHookContextOptions, PluginHookExecutionFinishedEntry,
    PluginHookExecutionStartedEntry, PluginHookExecutionStatus, PluginHookKind,
    PluginHookOutputArtifactRef, PluginHookOutputEnvelope, PluginHookOutputStream, PluginHookRef,
    PluginManifest, PluginManifestSnapshot, PluginSkillRef, PluginStateProjection,
    PluginTrustDecision, PluginTrustEntry, plugin_hook_output_context_items,
    plugin_manifest_digests_match, validate_plugin_capability_digest,
    validate_plugin_hook_schema_digest, validate_plugin_id, validate_plugin_manifest_digest,
    validate_plugin_version,
};
pub use projection::{
    AGENT_GRAPH_PROJECTION_SCHEMA_VERSION, DISPATCH_TRACE_PROJECTION_SCHEMA_VERSION,
    DispatchTraceEntry, DispatchTraceKind, DispatchTraceProjectionSnapshot, DispatchTraceStatus,
    DispatchTraceSummary, DispatchTraceUsageSummary, FILE_PROJECTION_STORE_SCHEMA_VERSION,
    FileProjectionStore, ProjectionPressureEvaluation, ProjectionPressureReason,
    ProjectionPressureSample, ProjectionPressureThresholds, ProjectionQueryContract,
    ProjectionQueryFamily, ProjectionQueryScope, ProjectionQuerySurface, ProjectionRebuildOutput,
    ProjectionRebuildReport, ProjectionStore, ProjectionStoreRecommendation, ProjectionStoreState,
    SESSION_LIST_PROJECTION_SCHEMA_VERSION, SessionListProjectionEntry,
    SessionListProjectionSnapshot, SessionListReadinessSummary, SessionListTaskSummary,
    SessionListUsageSummary, agent_graph_projection_from_records,
    dispatch_trace_projection_from_records, evaluate_projection_pressure,
    session_list_projection_from_records,
};
pub use provider::{
    AssistantMessageKind, BackgroundTaskHandle, BackgroundTaskStatus, CompletionRequest,
    MessageRole, ModelMessage, PrefixSnapshot, Provider, ProviderCapabilities, ProviderChunk,
    ProviderContinuationState, ReasoningArtifact, ReasoningEffort, ReasoningStreamSupport,
    ResponseHandle, SessionStats, ToolCall, ToolCallCompletionIdPolicy, ToolCallStreamAccumulator,
    UsageStats,
};
pub use provider_timeout::{
    ProviderStreamTimeoutState, ProviderTimeoutMetadata, ProviderTimeoutPhase,
    timeout_provider_request, timeout_provider_stream_next,
};
pub use resume::{
    JobId, JobIntentEntry, LeaseId, ResumeDisposition, ResumeJobProjection,
    ResumeJobStateProjection, StepLeaseEntry, StepLeaseHeartbeatEntry, StepLeaseStatus,
};
pub use secret::{REDACTED_SECRET, SecretRedactor};
pub use session::{
    CompactionPreview, CompactionRecord, ContextAssemblySkippedEntry, ControlEntry,
    DomainEventRecord, JsonlSessionStore, McpElicitationDecision, McpElicitationEntry,
    MemorySnapshot, Session, SessionLogEntry, SessionStreamRecord, ToolApprovalAllowSource,
    ToolApprovalAuditAction, ToolApprovalEntry, ToolApprovalSessionGrantEntry,
    ToolApprovalSessionGrantExpiry, ToolApprovalUserDecision, ToolEgressEntry, ToolExecutionEntry,
    ToolExecutionStatus, ToolSubjectAudit, TypedDomainEventRecord, latest_compaction_record,
    session_stats_from_entries,
};
pub use skill::{
    SkillDescriptor, SkillIndexSnapshot, SkillLoadEntry, SkillLoadState, SkillRunMode, SkillSource,
    SkillStateProjection, SkillTrustState,
};
pub use task::{
    AgentRole, DEFAULT_TASK_MAX_PLAN_VERSIONS, SessionRef, TASK_AGENT_DISPLAY_NAME_MAX_CHARS,
    TASK_PLAN_UPDATE_TOOL_NAME, TaskChildSessionDisplayNameEntry, TaskChildSessionEntry,
    TaskChildSessionStatus, TaskGraphProjection, TaskGraphStepProjection, TaskId,
    TaskIsolationMode, TaskPlanEntry, TaskPlanProjection, TaskPlanStatus, TaskPlanUpdateContext,
    TaskReadyDeferredReason, TaskReadyDeferredStep, TaskReadyQueue, TaskReadyQueueOptions,
    TaskRouteId, TaskRouteStatus, TaskRunEntry, TaskRunProjection, TaskRunStatus,
    TaskStateProjection, TaskStepEntry, TaskStepId, TaskStepMode, TaskStepProjection, TaskStepSpec,
    TaskStepStatus, TaskSubagentApprovalRouteEntry, TaskSubagentElicitationRouteEntry,
    child_session_ref, normalize_task_agent_display_name, task_plan_update_entry,
    task_plan_update_result_content, task_plan_update_tool_spec, validate_task_plan_graph_steps,
};
pub use task_memory::{
    AttemptRef, BranchId, CommandReceiptId, FileChangeRef, ModelAssistedMemoryDecision,
    ModelAssistedMemoryFact, ModelAssistedTaskMemorySummary, SourcedDecision, SourcedFact,
    TaskMemoryExtractionInput, TaskMemoryId, TaskMemoryV1, VerificationReceiptId,
    extract_task_memory_from_stream_records, task_memory_context_items,
};
pub use task_orchestrator::{
    SequentialTaskOrchestrator, SequentialTaskRequest, SequentialTaskRunOutput,
    SequentialTaskStepOutput, TaskChildChangeSetProposal, TaskChildSessionRunOutput,
    TaskChildSessionRunRequest, TaskChildSessionRunner, changeset_only_child_tool_registry,
    changeset_only_child_tool_scope, decode_changeset_only_child_output,
    validate_changeset_only_parent_snapshot_unchanged_for_task,
};
pub use terminal_task::{
    TerminalExecutionBackendCapabilities, TerminalExecutionBackendKind, TerminalTaskEntry,
    TerminalTaskHandle, TerminalTaskId, TerminalTaskProjection, TerminalTaskStatus,
    TerminalTaskSummary, terminal_cleanup_receipt_for_status,
};
pub use time::saturating_elapsed;
pub use tool::{
    ScopedToolRegistry, Tool, ToolAccess, ToolCategory, ToolContext, ToolDiffBudget, ToolDiffStats,
    ToolEgressAudit, ToolError, ToolErrorKind, ToolExecutionId, ToolPreview, ToolPreviewCapability,
    ToolPreviewFile, ToolPreviewFileSnapshot, ToolPreviewSnapshot, ToolProgressEvent,
    ToolProgressSink, ToolReceiptMetadata, ToolReceiptReplayDecision, ToolReceiptStatus,
    ToolRegistry, ToolRegistryScope, ToolResult, ToolResultMeta, ToolResultStatus,
    ToolResultSummary, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
};
pub use verification::{
    ArtifactId, CandidateCheck, ChangesetId, CheckCommand, CheckDiscoverySource, CheckPromotion,
    CheckSpec, CheckSpecId, CheckSpecRecordedEntry, ChildVerificationReceiptLinked,
    CompletionCriteria, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DiscoveredCheck,
    EnvironmentFingerprint, EvidenceReceipt, EvidenceScope, FileMetadataEvidence,
    FileMetadataPlatform, FileType, MAX_WORKSPACE_SNAPSHOT_FILE_BYTES,
    PluginVerificationHookReceiptRequest, ReadinessEvaluatedEntry, ReadinessEvaluation,
    ReadinessInput, ReadinessReason, ReceiptId, ReceiptStatus, RedactionState, RequiredAction,
    RunStatus, SandboxDecisionId, SandboxProfileHash, SandboxProfileRequirement,
    SnapshotEntryState, ToolCallId, ToolEffect, TrustedCheckSpec, VerificationAutoRunPolicy,
    VerificationBinding, VerificationCheckConfig, VerificationCheckRunEntry,
    VerificationCheckRunId, VerificationCheckRunRequest, VerificationCheckRunStatus,
    VerificationConfig, VerificationPolicy, VerificationPolicyChangedEntry, VerificationReceipt,
    VerificationRecordedEntry, VerificationScope, VerificationScopeConfig, VerificationScopeHash,
    VerificationScopeProfile, VerificationSkipDecision, VerificationStaleCause,
    VerificationStaleReason, VerificationStateProjection, VerificationStateProjectionSnapshot,
    VerificationVerdict, VisibleCompletionState, WorkspaceId, WorkspaceKnowledge,
    WorkspaceMutationEvidence, WorkspaceRevision, WorkspaceSnapshotBuild, WorkspaceSnapshotEntry,
    WorkspaceSnapshotId, WorkspaceSnapshotManifestV1, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    WorkspaceTrustRequirement, WorkspaceTrustSnapshotId, build_workspace_snapshot,
    build_workspace_snapshot_for_event, check_specs_from_user_config, default_scope_excludes,
    discover_candidate_checks, discover_candidate_checks_with_user_config, evaluate_readiness,
    record_plugin_verification_hook_receipt, run_verification_check, stable_workspace_id,
    verification_check_run_id,
};
pub use write_isolation::{
    IsolatedChangeSetProduced, IsolatedWorkspaceBackend, IsolatedWorkspaceCreated, MergeDecision,
    MergeReviewId, MergeReviewParentMutationOutcome, MergeReviewParentMutationRequest,
    MergeReviewRequested, MergeReviewResolved, MergeReviewState, WriteIsolationAgentId,
    WriteIsolationMode, WriteIsolationProjection, WriteIsolationRecordRef, WriteLeaseAcquired,
    WriteLeaseId, WriteLeaseReleaseStatus, WriteLeaseReleased, WriteLeaseScope, WriteLeaseState,
    resolve_merge_review_parent_mutation,
};
