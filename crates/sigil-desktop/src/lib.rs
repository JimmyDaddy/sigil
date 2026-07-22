//! Desktop-owned launcher and client boundary.
//!
//! The crate owns the local `sigil serve` child, per-launch bearer, bootstrap handshake, and
//! shutdown lifecycle. It deliberately does not depend on the kernel, runtime, TUI, or HTTP server
//! implementation. A future native shell can depend on this crate without exposing process or
//! credential primitives to its renderer.

mod client;
mod dto;
mod events;
mod launcher;
mod manager;
mod protocol;
mod secret;

pub use client::{DesktopClientError, DesktopHttpClient, DesktopRunEventStream};
pub use dto::{
    DesktopAgentActivityItem, DesktopAgentActivityStatus, DesktopAgentActivityView,
    DesktopAgentHandoffStatus, DesktopAgentUsageSummary, DesktopApplicationAgentBinding,
    DesktopApplicationAgentCatalogEntry, DesktopApplicationClientAction,
    DesktopApplicationCommandCatalogEntry, DesktopApplicationExtensionCatalog,
    DesktopApplicationModelOption, DesktopApplicationSkillBinding,
    DesktopApplicationSkillCatalogEntry, DesktopApprovalCommandReceipt, DesktopApprovalDecision,
    DesktopApprovalDecisionRecord, DesktopApprovalDecisionRequest, DesktopApprovalRecordedDecision,
    DesktopCatalogQuery, DesktopCheckpointFileAvailability, DesktopCheckpointFileView,
    DesktopCheckpointRestoreConflictReason, DesktopCheckpointRestoreKind,
    DesktopCheckpointRestorePreviewFile, DesktopCheckpointRestoreReceipt,
    DesktopCheckpointRestoreRequest, DesktopCheckpointRestoreReview, DesktopCheckpointReverseDiff,
    DesktopCommandEnvelope, DesktopCompactionAdmission, DesktopCompactionEconomics,
    DesktopCompactionReceipt, DesktopCompactionReview, DesktopContextWindowSource,
    DesktopContinuityRecoveryAction, DesktopConversationDisplayApprovalDecision,
    DesktopConversationDisplayAssistantPhase, DesktopConversationDisplayCheckpointConflictReason,
    DesktopConversationDisplayCheckpointOutcome, DesktopConversationDisplayContent,
    DesktopConversationDisplayGapFact, DesktopConversationDisplayGapKind,
    DesktopConversationDisplayItem, DesktopConversationDisplayItemKind,
    DesktopConversationDisplayMessageRole, DesktopConversationDisplayOrder,
    DesktopConversationDisplayPage, DesktopConversationDisplayQuery,
    DesktopConversationDisplaySource, DesktopConversationDisplayStatus,
    DesktopConversationForkPointView, DesktopConversationForkReceipt,
    DesktopConversationLiveProvisionalAnchor, DesktopConversationQueueBlockedReason,
    DesktopConversationQueueCommandAction, DesktopConversationQueueCommandActionKind,
    DesktopConversationQueueCommandReceipt, DesktopConversationQueueCommandRequest,
    DesktopConversationQueueGeneration, DesktopConversationQueueItem,
    DesktopConversationQueueItemKind, DesktopConversationQueueItemStatus,
    DesktopConversationQueuePromptMaterial, DesktopConversationQueueView,
    DesktopConversationRecoveryCommandAction, DesktopConversationRecoveryCommandActionKind,
    DesktopConversationRecoveryCommandReceipt, DesktopConversationRecoveryView,
    DesktopConversationTerminalFrontier, DesktopDurableSessionFrontier, DesktopForegroundRunOwner,
    DesktopModelSelectionPolicy, DesktopPendingApproval, DesktopPermissionMode,
    DesktopReasoningEffort, DesktopRunCancelCommandReceipt, DesktopRunCancelRequest,
    DesktopRunContextView, DesktopRunSnapshot, DesktopRunStartCommandReceipt,
    DesktopRunStartRequest, DesktopRunStatus, DesktopSessionCatalogBatchAction,
    DesktopSessionCatalogBatchExecuteRequest, DesktopSessionCatalogBatchItem,
    DesktopSessionCatalogBatchOutcome, DesktopSessionCatalogBatchPlan,
    DesktopSessionCatalogBatchPlanItem, DesktopSessionCatalogBatchPlanRequest,
    DesktopSessionCatalogBatchPlanStatus, DesktopSessionCatalogBatchReceipt,
    DesktopSessionCatalogBatchReceiptItem, DesktopSessionCatalogEntry, DesktopSessionCatalogPage,
    DesktopSessionCatalogState, DesktopSessionContinuityView, DesktopSessionCreateRequest,
    DesktopSessionDeleteRequest, DesktopSessionInvalidSourceDeleteReceipt,
    DesktopSessionInvalidSourceDeleteRequest, DesktopSessionListResponse,
    DesktopSessionMutationReceipt, DesktopSessionOpenRequest, DesktopSessionQuarantineReceipt,
    DesktopSessionQuarantineRequest, DesktopSessionRenameRequest, DesktopSessionSnapshot,
    DesktopSessionTranscriptMessage, DesktopSessionTranscriptPage, DesktopSupportBundleExport,
    DesktopSupportCheck, DesktopSupportDoctorReport, DesktopSupportEnvironment,
    DesktopSupportPrivacy, DesktopSupportStatus, DesktopSupportSummary,
    DesktopTranscriptAssistantKind, DesktopTranscriptQuery, DesktopTranscriptRole,
    DesktopVerificationAction, DesktopVerificationCheckStatus, DesktopVerificationEvidence,
    DesktopVerificationRecommendationKind, DesktopVerificationRerunCommandReceipt,
    DesktopVerificationRerunRequest, DesktopVerificationScope, DesktopVerificationVerdict,
    DesktopVerificationView,
};
pub use events::{
    DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION, DESKTOP_PUBLIC_RUN_EVENT_SCHEMA_VERSION,
    DesktopProtocolEvent, DesktopProtocolEventClass, DesktopProtocolEventError,
    DesktopPublicRunEvent, DesktopTimelineApproval, DesktopTimelineEvent, DesktopTimelineEventKind,
};
pub use launcher::{
    DesktopLaunchError, DesktopLaunchRequest, DesktopLauncher, DesktopServerProcess,
    DesktopShutdownError, DesktopShutdownKind, DesktopShutdownReport,
};
pub use manager::{
    DesktopConnectionState, DesktopWorkspaceManager, DesktopWorkspaceManagerError,
    DesktopWorkspaceOpenRequest, DesktopWorkspaceSummary,
};
pub use protocol::{DesktopServerAuthentication, DesktopServerCapabilities, DesktopServerInfo};
