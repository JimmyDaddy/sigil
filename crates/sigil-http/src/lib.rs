#![recursion_limit = "1024"]

mod auth;
mod command_store;
mod config;
mod disclosure;
mod driver;
mod dto;
mod durable_io;
mod journal;
mod listener;
mod openapi;
mod production_driver;
mod protocol;
mod registry;
mod session_catalog_batch;
mod sse;
mod support;

pub use auth::{HttpAuthError, HttpAuthValidator};
pub use command_store::{HttpCommandStoreError, HttpDurableCommandStore};
pub use config::{DEFAULT_HTTP_TOKEN_ENV, HttpAuthConfig, HttpServerConfig, HttpServerConfigError};
pub use disclosure::{
    HTTP_EGRESS_DISCLOSURE_SCHEMA_VERSION, HttpDisclosureReplayError, HttpDurableDisclosureError,
    HttpDurableEgressDisclosureJournal, HttpDurableEgressDisclosurePresenter,
    HttpDurableEgressDisclosureRecord, HttpEgressDisclosureEvent, HttpEgressDisclosureReplayBuffer,
    HttpEgressDisclosureReplayError, HttpReplayEgressDisclosurePresenter,
};
pub use driver::{
    HttpConversationDisplayDriverError, HttpConversationQueueDriverCommand,
    HttpConversationQueueDriverError, HttpConversationRecoveryDriverCommand,
    HttpConversationRecoveryDriverError, HttpConversationRecoveryDriverOutput,
    HttpDurableSessionAttachmentGuard, HttpIntentStackDriverError, HttpQueuedRunAdmission,
    HttpQueuedRunDriverStart, HttpRunAdmissionError, HttpRunDriver, HttpRunDriverApproval,
    HttpRunDriverCancel, HttpRunDriverError, HttpRunDriverErrorKind, HttpRunDriverStart,
    HttpRunDriverTaskPause, HttpRunDriverTerminalTaskCancel, HttpSessionOpenBindingError,
    HttpToolArtifactReadDriverError, HttpUserInputDecisionDriverCommand,
};
pub use dto::{
    HTTP_CONVERSATION_QUEUE_SCHEMA_VERSION, HTTP_MAX_CONVERSATION_QUEUE_ITEMS,
    HTTP_SERVER_INFO_SCHEMA_VERSION, HTTP_TOOL_ARTIFACT_PAGE_SCHEMA_VERSION, HttpAgentActivityItem,
    HttpAgentActivityStatus, HttpAgentActivityView, HttpAgentHandoffStatus, HttpAgentUsageSummary,
    HttpApplicationAgentBinding, HttpApplicationAgentCatalogEntry, HttpApplicationClientAction,
    HttpApplicationCommandCatalogEntry, HttpApplicationExtensionCatalog,
    HttpApplicationModelOption, HttpApplicationSkillBinding, HttpApplicationSkillCatalogEntry,
    HttpApprovalCommandReceipt, HttpApprovalDecision, HttpApprovalDecisionRecord,
    HttpApprovalDecisionRequest, HttpApprovalLifecycleState, HttpApprovalLifecycleView,
    HttpApprovalRouteState, HttpCheckpointFileAvailability, HttpCheckpointFileView,
    HttpCheckpointRestoreConflictReason, HttpCheckpointRestoreKind,
    HttpCheckpointRestorePreviewFile, HttpCheckpointRestoreReceipt, HttpCheckpointRestoreRequest,
    HttpCheckpointRestoreReview, HttpCheckpointReverseDiff, HttpCheckpointView,
    HttpCompactionAdmission, HttpCompactionEconomics, HttpCompactionReceipt, HttpCompactionReview,
    HttpContextWindowSource, HttpContinuityRecoveryAction, HttpConversationDisplayApprovalDecision,
    HttpConversationDisplayAssistantPhase, HttpConversationDisplayCheckpointConflictReason,
    HttpConversationDisplayCheckpointOutcome, HttpConversationDisplayContent,
    HttpConversationDisplayGapFact, HttpConversationDisplayGapKind, HttpConversationDisplayItem,
    HttpConversationDisplayItemKind, HttpConversationDisplayMessageRole,
    HttpConversationDisplayOrder, HttpConversationDisplayPage, HttpConversationDisplaySource,
    HttpConversationDisplayStatus, HttpConversationForkPointView, HttpConversationForkReceipt,
    HttpConversationLiveProvisionalAnchor, HttpConversationQueueBlockedReason,
    HttpConversationQueueCommandAction, HttpConversationQueueCommandActionKind,
    HttpConversationQueueCommandReceipt, HttpConversationQueueCommandRequest,
    HttpConversationQueueGeneration, HttpConversationQueueItem, HttpConversationQueueItemKind,
    HttpConversationQueueItemStatus, HttpConversationQueuePromptMaterial,
    HttpConversationQueueView, HttpConversationRecoveryCommandAction,
    HttpConversationRecoveryCommandActionKind, HttpConversationRecoveryCommandReceipt,
    HttpConversationRecoveryView, HttpConversationTaskControl, HttpConversationTaskLane,
    HttpConversationTaskPlanStep, HttpConversationTerminalFrontier, HttpDurableSessionFrontier,
    HttpForegroundRunOwner, HttpIntegrationLaneCandidateKind, HttpIntegrationPromotionTargetKind,
    HttpIntentDropCommandReceipt, HttpIntentDropExecution, HttpIntentDropPreview,
    HttpIntentDropPreviewRequest, HttpIntentDropRequest, HttpIntentStackView,
    HttpModelSelectionPolicy, HttpPendingApproval, HttpPendingApprovalDisplay,
    HttpPendingApprovalSubject, HttpPermissionMode, HttpPlanAction, HttpPlanDecisionAction,
    HttpPlanDecisionCommandReceipt, HttpPlanDecisionRequest, HttpPlanReview, HttpPlanReviewDetail,
    HttpPlanReviewSource, HttpPlanReviewStatus, HttpProviderConfigMode,
    HttpProviderConnectionEntry, HttpProviderConnectionInventory, HttpProviderConnectionIssue,
    HttpProviderConnectionReadiness, HttpProviderCredentialSource,
    HttpProviderDefaultModelSaveRequest, HttpProviderDefaultModelSaveResult, HttpProviderModelRef,
    HttpProviderSetupCatalog, HttpProviderSetupCatalogRequest, HttpProviderSetupCredentialSource,
    HttpProviderSetupModel, HttpProviderSetupProtocol, HttpProviderSetupSaveRequest,
    HttpProviderSetupSaveResult, HttpProviderSetupTemplate, HttpReasoningEffort,
    HttpRunCancelCommandReceipt, HttpRunCancelRequest, HttpRunContextView, HttpRunSnapshot,
    HttpRunStartCommandReceipt, HttpRunStartRequest, HttpRunStatus, HttpRunTerminalOutcome,
    HttpServerAuthentication, HttpServerCapabilities, HttpServerInfo, HttpSessionBinding,
    HttpSessionCatalogBatchAction, HttpSessionCatalogBatchExecuteRequest,
    HttpSessionCatalogBatchItem, HttpSessionCatalogBatchOutcome, HttpSessionCatalogBatchPlan,
    HttpSessionCatalogBatchPlanItem, HttpSessionCatalogBatchPlanRequest,
    HttpSessionCatalogBatchPlanStatus, HttpSessionCatalogBatchReceipt,
    HttpSessionCatalogBatchReceiptItem, HttpSessionContinuityView, HttpSessionCreateRequest,
    HttpSessionDeleteRequest, HttpSessionInvalidSourceDeleteReceipt,
    HttpSessionInvalidSourceDeleteRequest, HttpSessionMutationReceipt, HttpSessionOpenRequest,
    HttpSessionQuarantineReceipt, HttpSessionQuarantineRequest, HttpSessionRenameRequest,
    HttpSessionRouteRecoveryAction, HttpSessionRouteRecoveryCode, HttpSessionRouteRecoveryView,
    HttpSessionRouteTransitionKind, HttpSessionRouteTransitionView, HttpSessionSnapshot,
    HttpSessionTranscriptMessage, HttpSessionTranscriptPage, HttpSupportBundleExport,
    HttpSupportCheck, HttpSupportDoctorReport, HttpSupportEnvironment, HttpSupportPrivacy,
    HttpSupportStatus, HttpSupportSummary, HttpTaskContinuationRequest,
    HttpTaskIntegrationAcceptanceCommandReceipt, HttpTaskIntegrationAcceptanceView,
    HttpTaskIntegrationLaneView, HttpTaskIntegrationReviewRequest, HttpTaskIntegrationReviewView,
    HttpTaskPauseCommandReceipt, HttpTaskPauseRequest, HttpTerminalLifecycleView,
    HttpTerminalTaskCancelCommandReceipt, HttpTerminalTaskCancelRequest, HttpToolArtifactPage,
    HttpToolArtifactPageEncoding, HttpToolArtifactReadRequest, HttpToolArtifactSelector,
    HttpToolOutputShrinkReceipt, HttpTranscriptAssistantKind, HttpTranscriptRole,
    HttpUserInputDecisionCommandReceipt, HttpUserInputDecisionRequest, HttpUserInputRequest,
    HttpVerificationRerunCommandReceipt, HttpVerificationRerunRequest, HttpVerificationView,
};
pub use journal::{HttpDurableProtocolJournal, HttpProtocolJournalError};
pub use listener::{HttpListenerError, HttpLocalServer};
pub use openapi::{HTTP_OPENAPI_VERSION, http_openapi_document};
pub use production_driver::{HttpProductionRunDriver, HttpProductionRunDriverOptions};
pub use protocol::{HTTP_PROTOCOL_VERSION, HttpCommandEnvelope, HttpProtocolVersionError};
pub use registry::{HttpRegistryActivity, HttpRegistryError, HttpSessionRunRegistry};
pub use sse::{
    HTTP_PROTOCOL_EVENT_SCHEMA_VERSION, HTTP_RUN_EVENT_SSE_NAME, HttpDurableEventView,
    HttpEventPublishError, HttpLiveEventBus, HttpLiveEventRecvError, HttpLiveEventSubscriber,
    HttpProtocolCursor, HttpProtocolCursorError, HttpProtocolEvent, HttpProtocolEventBuffer,
    HttpProtocolEventClass, HttpProtocolEventView, HttpProtocolReplayError, HttpRunEventSequencer,
    HttpSseError, HttpSseEvent, HttpTransientEventView, public_run_event_to_sse,
};
pub use support::HttpSupportContext;

#[cfg(test)]
#[path = "tests/lib_tests.rs"]
mod tests;
