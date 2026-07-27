import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type { AppearanceSnapshot, ThemePreference } from "./appearance/contract";

import type {
  AgentActivitySummary,
  AgentBinding,
  CatalogPage,
  CatalogRequest,
  ConversationContinuity,
  ConversationQueueCommandInput,
  ConversationQueueCommandReceipt,
  ConversationQueueView,
  CompactionReview,
  CheckpointRestorePreviewInput,
  CheckpointRestoreReview,
  ConversationRecoveryCommandInput,
  ConversationRecoveryCommandReceipt,
  ConversationRecoveryView,
  ConversationDisplayPage,
  ConversationDisplayRequest,
  DesktopBootstrap,
  SessionOpenInput,
  SessionCatalogBatchExecuteInput,
  SessionCatalogBatchPlan,
  SessionCatalogBatchPlanInput,
  SessionCatalogBatchReceipt,
  SessionDeleteInput,
  SessionInvalidSourceDeleteInput,
  SessionInvalidSourceDeleteSummary,
  SessionMutationSummary,
  SessionQuarantineInput,
  SessionQuarantineSummary,
  SessionRenameInput,
  SessionSummary,
  RunAttachInput,
  RunStreamStatus,
  RunAttachment,
  PermissionMode,
  ProviderConnectionInventory,
  ProviderLegacyMigrationResult,
  ProviderModelRef,
  ProviderSetupCatalog,
  ProviderSetupCatalogInput,
  ProviderSetupSaveInput,
  ProviderSetupSaveResult,
  ReasoningEffort,
  SkillBinding,
  RunContext,
  RunSummary,
  TimelineEvent,
  WorkspaceSelection,
  WorkspaceSummary,
  ApprovalDecisionSummary,
  TimelineApproval,
  ApprovalAction,
  TranscriptPage,
  TranscriptRequest,
  VerificationRerunBinding,
  VerificationSummary,
  TaskIntegrationAcceptance,
  TaskIntegrationReview,
  TaskIntegrationReviewBinding,
  IntentDropBinding,
  IntentDropExecution,
  IntentDropPreview,
  IntentStackState,
  IntentVersionRef,
  TaskPauseBinding,
  SupportDoctorReport,
  SupportSaveSummary,
} from "./types";

export interface DesktopBridge {
  bootstrap(): Promise<DesktopBootstrap>;
  setAppearance(preference: ThemePreference): Promise<AppearanceSnapshot>;
  openExternalUrl(url: string): Promise<void>;
  supportDoctor(workspaceId: string): Promise<SupportDoctorReport>;
  exportSupportBundle(workspaceId: string): Promise<SupportSaveSummary>;
  providerConnections(workspaceId: string): Promise<ProviderConnectionInventory>;
  migrateLegacyProviderConnections(
    workspaceId: string,
    expectedRevision: string,
  ): Promise<ProviderLegacyMigrationResult>;
  recheckLegacyProviderMigration(
    workspaceId: string,
  ): Promise<ProviderConnectionInventory>;
  providerSetupCatalog(
    workspaceId: string,
    input: ProviderSetupCatalogInput,
  ): Promise<ProviderSetupCatalog>;
  saveProviderSetup(
    workspaceId: string,
    input: ProviderSetupSaveInput,
  ): Promise<ProviderSetupSaveResult>;
  pickWorkspace(): Promise<WorkspaceSelection>;
  openRecentWorkspace(recentId: string): Promise<WorkspaceSummary>;
  closeWorkspace(workspaceId: string, confirmActiveRuns?: boolean): Promise<WorkspaceSummary[]>;
  catalog(workspaceId: string, request: CatalogRequest): Promise<CatalogPage>;
  createSession(
    workspaceId: string,
    label?: string,
    modelRef?: ProviderModelRef,
  ): Promise<SessionSummary>;
  openSession(
    workspaceId: string,
    input: SessionOpenInput,
  ): Promise<SessionSummary>;
  renameSession(workspaceId: string, input: SessionRenameInput): Promise<SessionMutationSummary>;
  deleteSession(workspaceId: string, input: SessionDeleteInput): Promise<SessionMutationSummary>;
  quarantineSession(
    workspaceId: string,
    input: SessionQuarantineInput,
  ): Promise<SessionQuarantineSummary>;
  deleteInvalidSessionSource(
    workspaceId: string,
    input: SessionInvalidSourceDeleteInput,
  ): Promise<SessionInvalidSourceDeleteSummary>;
  planSessionCatalogBatch(
    workspaceId: string,
    input: SessionCatalogBatchPlanInput,
  ): Promise<SessionCatalogBatchPlan>;
  executeSessionCatalogBatch(
    workspaceId: string,
    input: SessionCatalogBatchExecuteInput,
  ): Promise<SessionCatalogBatchReceipt>;
  transcript(
    workspaceId: string,
    sessionId: string,
    request: TranscriptRequest,
  ): Promise<TranscriptPage>;
  display(
    workspaceId: string,
    sessionId: string,
    request: ConversationDisplayRequest,
  ): Promise<ConversationDisplayPage>;
  continuity(workspaceId: string, sessionId: string): Promise<ConversationContinuity>;
  conversationQueue(workspaceId: string, sessionId: string): Promise<ConversationQueueView>;
  commandConversationQueue(
    workspaceId: string,
    input: ConversationQueueCommandInput,
  ): Promise<ConversationQueueCommandReceipt>;
  conversationRecovery(workspaceId: string, sessionId: string): Promise<ConversationRecoveryView>;
  conversationCompactionPreview(workspaceId: string, sessionId: string): Promise<CompactionReview>;
  checkpointRestorePreview(
    workspaceId: string,
    input: CheckpointRestorePreviewInput,
  ): Promise<CheckpointRestoreReview>;
  commandConversationRecovery(
    workspaceId: string,
    input: ConversationRecoveryCommandInput,
  ): Promise<ConversationRecoveryCommandReceipt>;
  runContext(workspaceId: string, sessionId: string): Promise<RunContext>;
  agentActivity(workspaceId: string, sessionId: string): Promise<AgentActivitySummary>;
  startRun(
    workspaceId: string,
    sessionId: string,
    prompt: string,
    permissionMode: PermissionMode,
    modelName?: string,
    modelSelectionBinding?: string,
    reasoningEffort?: ReasoningEffort,
    reasoningEffortBinding?: string,
    skillBinding?: SkillBinding,
    agentBinding?: AgentBinding,
  ): Promise<RunSummary>;
  continueTask(
    workspaceId: string,
    sessionId: string,
    taskId: string,
    permissionMode: PermissionMode,
    guidance?: string,
  ): Promise<RunSummary>;
  attachRun(workspaceId: string, input: RunAttachInput): Promise<RunAttachment>;
  cancelRun(workspaceId: string, sessionId: string, runId: string): Promise<RunSummary>;
  pauseTask(
    workspaceId: string,
    sessionId: string,
    runId: string,
    request: TaskPauseBinding,
  ): Promise<RunSummary>;
  resolveApproval(
    workspaceId: string,
    sessionId: string,
    runId: string,
    approval: TimelineApproval,
    decision: ApprovalAction,
  ): Promise<ApprovalDecisionSummary>;
  verification(workspaceId: string, sessionId: string): Promise<VerificationSummary>;
  rerunVerification(
    workspaceId: string,
    sessionId: string,
    request: VerificationRerunBinding,
  ): Promise<VerificationSummary>;
  taskIntegrationReview(
    workspaceId: string,
    sessionId: string,
  ): Promise<TaskIntegrationReview>;
  acceptTaskIntegration(
    workspaceId: string,
    sessionId: string,
    request: TaskIntegrationReviewBinding,
  ): Promise<TaskIntegrationAcceptance>;
  intentStack(workspaceId: string, sessionId: string): Promise<IntentStackState>;
  previewIntentDrop(
    workspaceId: string,
    sessionId: string,
    intentRef: IntentVersionRef,
  ): Promise<IntentDropPreview>;
  executeIntentDrop(
    workspaceId: string,
    sessionId: string,
    request: IntentDropBinding,
  ): Promise<IntentDropExecution>;
  subscribeRunEvents(listener: (event: TimelineEvent) => void): Promise<() => void>;
  subscribeRunStreamStatus(listener: (status: RunStreamStatus) => void): Promise<() => void>;
  subscribeAppearance(listener: (snapshot: AppearanceSnapshot) => void): Promise<() => void>;
}

export const desktopBridge: DesktopBridge = {
  bootstrap: () => invoke<DesktopBootstrap>("desktop_bootstrap"),
  setAppearance: (preference) =>
    invoke<AppearanceSnapshot>("desktop_set_appearance", { input: { preference } }),
  openExternalUrl: (url) =>
    invoke<void>("desktop_open_external_url", { input: { url } }),
  supportDoctor: (workspaceId) =>
    invoke<SupportDoctorReport>("desktop_support_doctor", { workspaceId }),
  exportSupportBundle: (workspaceId) =>
    invoke<SupportSaveSummary>("desktop_export_support_bundle", { workspaceId }),
  providerConnections: (workspaceId) =>
    invoke<ProviderConnectionInventory>("desktop_provider_connections", { workspaceId }),
  migrateLegacyProviderConnections: (workspaceId, expectedRevision) =>
    invoke<ProviderLegacyMigrationResult>("desktop_migrate_legacy_provider_connections", {
      workspaceId,
      expectedRevision,
    }),
  recheckLegacyProviderMigration: (workspaceId) =>
    invoke<ProviderConnectionInventory>("desktop_recheck_legacy_provider_migration", {
      workspaceId,
    }),
  providerSetupCatalog: (workspaceId, input) =>
    invoke<ProviderSetupCatalog>("desktop_provider_setup_catalog", { workspaceId, input }),
  saveProviderSetup: (workspaceId, input) =>
    invoke<ProviderSetupSaveResult>("desktop_save_provider_setup", { workspaceId, input }),
  pickWorkspace: () =>
    invoke<WorkspaceSelection>("desktop_pick_workspace"),
  openRecentWorkspace: (recentId) =>
    invoke<WorkspaceSummary>("desktop_open_recent_workspace", { recentId }),
  closeWorkspace: (workspaceId, confirmActiveRuns = false) =>
    invoke<WorkspaceSummary[]>("desktop_close_workspace", {
      workspaceId,
      confirmActiveRuns,
    }),
  catalog: (workspaceId, request) =>
    invoke<CatalogPage>("desktop_catalog", { workspaceId, request }),
  createSession: (workspaceId, label, modelRef) =>
    invoke<SessionSummary>("desktop_create_session", {
      workspaceId,
      input: { label, modelRef },
    }),
  openSession: (workspaceId, input) =>
    invoke<SessionSummary>("desktop_open_session", { workspaceId, input }),
  renameSession: (workspaceId, input) =>
    invoke<SessionMutationSummary>("desktop_rename_session", { workspaceId, input }),
  deleteSession: (workspaceId, input) =>
    invoke<SessionMutationSummary>("desktop_delete_session", { workspaceId, input }),
  quarantineSession: (workspaceId, input) =>
    invoke<SessionQuarantineSummary>("desktop_quarantine_session", { workspaceId, input }),
  deleteInvalidSessionSource: (workspaceId, input) =>
    invoke<SessionInvalidSourceDeleteSummary>("desktop_delete_invalid_session_source", {
      workspaceId,
      input,
    }),
  planSessionCatalogBatch: (workspaceId, input) =>
    invoke<SessionCatalogBatchPlan>("desktop_plan_session_catalog_batch", {
      workspaceId,
      input,
    }),
  executeSessionCatalogBatch: (workspaceId, input) =>
    invoke<SessionCatalogBatchReceipt>("desktop_execute_session_catalog_batch", {
      workspaceId,
      input,
    }),
  transcript: (workspaceId, sessionId, request) =>
    invoke<TranscriptPage>("desktop_transcript", {
      workspaceId,
      sessionId,
      request,
    }),
  display: (workspaceId, sessionId, request) =>
    invoke<ConversationDisplayPage>("desktop_display", {
      workspaceId,
      sessionId,
      request,
    }),
  continuity: (workspaceId, sessionId) =>
    invoke<ConversationContinuity>("desktop_continuity", { workspaceId, sessionId }),
  conversationQueue: (workspaceId, sessionId) =>
    invoke<ConversationQueueView>("desktop_conversation_queue", { workspaceId, sessionId }),
  commandConversationQueue: (workspaceId, input) =>
    invoke<ConversationQueueCommandReceipt>("desktop_command_conversation_queue", {
      workspaceId,
      input,
    }),
  conversationRecovery: (workspaceId, sessionId) =>
    invoke<ConversationRecoveryView>("desktop_conversation_recovery", { workspaceId, sessionId }),
  conversationCompactionPreview: (workspaceId, sessionId) =>
    invoke<CompactionReview>("desktop_conversation_compaction_preview", { workspaceId, sessionId }),
  checkpointRestorePreview: (workspaceId, input) =>
    invoke<CheckpointRestoreReview>("desktop_checkpoint_restore_preview", { workspaceId, input }),
  commandConversationRecovery: (workspaceId, input) =>
    invoke<ConversationRecoveryCommandReceipt>("desktop_command_conversation_recovery", {
      workspaceId,
      input,
    }),
  runContext: (workspaceId, sessionId) =>
    invoke<RunContext>("desktop_run_context", { workspaceId, sessionId }),
  agentActivity: (workspaceId, sessionId) =>
    invoke<AgentActivitySummary>("desktop_agent_activity", { workspaceId, sessionId }),
  startRun: (
    workspaceId,
    sessionId,
    prompt,
    permissionMode,
    modelName,
    modelSelectionBinding,
    reasoningEffort,
    reasoningEffortBinding,
    skillBinding,
    agentBinding,
  ) =>
    invoke<RunSummary>("desktop_start_run", {
      workspaceId,
      input: {
        sessionId,
        prompt,
        permissionMode,
        modelName,
        modelSelectionBinding,
        reasoningEffort,
        reasoningEffortBinding,
        skillBinding,
        agentBinding,
      },
    }),
  continueTask: (workspaceId, sessionId, taskId, permissionMode, guidance) =>
    invoke<RunSummary>("desktop_continue_task", {
      workspaceId,
      input: { sessionId, taskId, permissionMode, guidance },
    }),
  attachRun: (workspaceId, input) =>
    invoke<RunAttachment>("desktop_attach_run", {
      workspaceId,
      input,
    }),
  cancelRun: (workspaceId, sessionId, runId) =>
    invoke<RunSummary>("desktop_cancel_run", {
      workspaceId,
      input: { sessionId, runId },
    }),
  pauseTask: (workspaceId, sessionId, runId, request) =>
    invoke<RunSummary>("desktop_pause_task", {
      workspaceId,
      input: { sessionId, runId, taskId: request.taskId, planVersion: request.planVersion },
    }),
  resolveApproval: (workspaceId, sessionId, runId, approval, decision) =>
    invoke<ApprovalDecisionSummary>("desktop_resolve_approval", {
      workspaceId,
      input: {
        sessionId,
        runId,
        callId: approval.callId,
        approvalRequestId: approval.approvalRequestId,
        toolCallHash: approval.toolCallHash,
        policyVersion: approval.policyVersion,
        expiresAtMs: approval.expiresAtMs,
        decision,
      },
    }),
  verification: (workspaceId, sessionId) =>
    invoke<VerificationSummary>("desktop_verification", { workspaceId, sessionId }),
  rerunVerification: (workspaceId, sessionId, request) =>
    invoke<VerificationSummary>("desktop_rerun_verification", {
      workspaceId,
      input: { sessionId, request },
    }),
  taskIntegrationReview: (workspaceId, sessionId) =>
    invoke<TaskIntegrationReview>("desktop_task_integration_review", {
      workspaceId,
      sessionId,
    }),
  acceptTaskIntegration: (workspaceId, sessionId, request) =>
    invoke<TaskIntegrationAcceptance>("desktop_accept_task_integration", {
      workspaceId,
      input: { sessionId, request },
    }),
  intentStack: (workspaceId, sessionId) =>
    invoke<IntentStackState>("desktop_intent_stack", { workspaceId, sessionId }),
  previewIntentDrop: (workspaceId, sessionId, intentRef) =>
    invoke<IntentDropPreview>("desktop_preview_intent_drop", {
      workspaceId,
      input: { sessionId, intentRef },
    }),
  executeIntentDrop: (workspaceId, sessionId, request) =>
    invoke<IntentDropExecution>("desktop_execute_intent_drop", {
      workspaceId,
      input: { sessionId, request },
    }),
  subscribeRunEvents: async (listener) =>
    listen<TimelineEvent>("sigil-run-event", (event) => listener(event.payload)),
  subscribeRunStreamStatus: async (listener) =>
    listen<RunStreamStatus>("sigil-run-stream-status", (event) => listener(event.payload)),
  subscribeAppearance: async (listener) =>
    listen<AppearanceSnapshot>("sigil-appearance-changed", (event) => listener(event.payload)),
};
