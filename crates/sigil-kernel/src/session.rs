use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ExternalProvenanceEntry, MemoryConfig, MemoryLoadReport,
    agent_thread::{
        AgentApprovalRouteEntry, AgentElicitationRouteEntry, AgentMailboxMessageEntry,
        AgentMergeSafePointEntry, AgentProfileCapturedEntry, AgentProfilePolicyEntry,
        AgentProfilePolicyProjection, AgentProfileTrustEntry, AgentProfileTrustProjection,
        AgentResultContinuationEntry, AgentResultContinuationProjection, AgentRouteClosedEntry,
        AgentRunAttemptStartedEntry, AgentRunHeartbeatEntry, AgentRunInterruptedEntry,
        AgentThreadClosedEntry, AgentThreadDisplayNameEntry, AgentThreadMessageRoutedEntry,
        AgentThreadResultDeliveredEntry, AgentThreadResultRecordedEntry, AgentThreadStartedEntry,
        AgentThreadStateProjection, AgentThreadStatusChangedEntry, closed_agent_routes,
        interrupted_agent_attempts, interrupted_agent_mailbox_messages,
    },
    changeset::{ChangeSet, ChangeSetProjection, ChangeSetResult},
    context_engine::{
        ContextBodyRef, ContextInclusionReason, ContextItem, ContextItemId, ContextPackOptions,
        ContextSensitivity, ContextSource, ContextTrustLevel,
        DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES, PackedContext, RuntimeContextCandidates,
        SessionArchive, SessionArchiveEntry, context_provenance_row_v1,
        estimate_context_token_cost, pack_context_items, validate_context_render_snippet,
    },
    conversation_queue::{
        ConversationInputEditedEntry, ConversationInputPromotedEntry,
        ConversationInputQueueControlEntry, ConversationInputQueuedEntry,
        ConversationInputReorderedEntry, ConversationInputStatusEntry,
        ConversationQueueDurableProjection, ConversationQueueProjection,
    },
    event::{
        DomainEvent, DurableEventPayloadStorage, DurableEventType, EventClass, EventSyncClass,
        ProjectionApplyDecision, ProjectionCursor, StoredEvent, StoredEventDecode,
        TypedDomainEvent, TypedStoredEventDecode, decode_stored_event, decode_typed_stored_event,
        projection_apply_decision_for_record, stable_event_hash, stable_event_uuid,
    },
    memory::{apply_memory_report, materialize_memory},
    mutation::{ExecutionMutationProfile, MutationEventRecorder},
    permission::{
        ApprovalMode, PathTrustZone, PermissionConfirmation, PermissionRisk,
        ToolApprovalSessionGrantFacet, ToolApprovalSessionGrantScope, ToolOperation,
    },
    plan::{
        PlanApprovalProjection, PlanApprovedEntry, PlanArtifactProjection,
        PlanDecisionRecordedEntry, PlanDraftCreatedEntry, PlanPermissionGrantedEntry,
        TaskCreatedFromPlanEntry,
    },
    plugin::{
        PluginHookExecutionFinishedEntry, PluginHookExecutionStartedEntry, PluginManifestSnapshot,
        PluginStateProjection, PluginTrustEntry,
    },
    provider::{
        CompletionRequest, MessageRole, ModelMessage, PrefixSnapshot, ProviderContinuationState,
        ResponseHandle, SessionStats, UsageStats,
    },
    skill::{SkillIndexSnapshot, SkillLoadEntry, SkillStateProjection},
    task::{
        TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskPlanEntry, TaskRunEntry,
        TaskStateProjection, TaskStepEntry, TaskSubagentApprovalRouteEntry,
        TaskSubagentElicitationRouteEntry,
    },
    task_memory::{TaskMemoryV1, task_memory_context_items},
    terminal_task::{TerminalTaskEntry, TerminalTaskProjection},
    tool::{
        NetworkEffect, ToolAccess, ToolError, ToolErrorKind, ToolPreviewSnapshot, ToolResult,
        ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
    },
    verification::{
        CheckSpecRecordedEntry, ChildVerificationReceiptLinked, ReadinessEvaluatedEntry,
        VerificationCheckRunEntry, VerificationFailureLocatorRecorded,
        VerificationPolicyChangedEntry, VerificationReceiptLinkRecorded, VerificationRecordedEntry,
        VerificationStateProjection, WorkspaceTrustDecisionEntry,
    },
    write_isolation::{
        IsolatedChangeSetProduced, IsolatedWorkspaceCreated, MergeReviewRequested,
        MergeReviewResolved, WriteIsolationProjection, WriteLeaseAcquired, WriteLeaseReleased,
    },
};

static SESSION_LOG_IO_LOCK: Mutex<()> = Mutex::new(());
const SESSION_LOG_SHARED_LOCK_RETRIES: usize = 50;
const SESSION_LOG_SHARED_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const REQUEST_CONTEXT_V0_MAX_TOKENS: usize = 512;
const REQUEST_CONTEXT_V0_SESSION_ARCHIVE_LIMIT: usize = 4;
const REQUEST_CONTEXT_V0_EXTERNAL_SOURCE_LIMIT: usize = 64;
const REQUEST_CONTEXT_V0_ENTRY_MAX_BYTES: usize = 2048;
const REQUEST_CONTEXT_V0_ENTRY_OVERLAP_BYTES: usize = 256;
const UNSAFE_EXTERNAL_RECOVERY_AUDIT_REASON: &str =
    "recovery skipped unsafe external persistence control";

mod compaction_plan;
mod compaction_shrink_sidecar;
mod compaction_sidecar;
mod compaction_v2;
mod context;
mod context_projection;
mod conversation_queue_promotion;
mod entry;
mod facade;
mod portable_compaction;
mod projection;
mod provider_attempt;
mod provider_continuation;
mod provider_continuation_activation;
mod provider_continuation_admission;
mod provider_continuation_invalidation_coordinator;
mod provider_continuation_payload_coordinator;
mod provider_continuation_payload_store;
mod provider_continuation_resolution_coordinator;
mod provider_native_compaction;
mod recovery;
mod stats;
mod store;
mod tool_output_projection;
mod writer;

pub use compaction_plan::{
    COMPACTION_FOLD_PLAN_SCHEMA_VERSION, CompactionEventRef, CompactionFoldPlan,
    CompactionFoldProtectionReason, ProtectedCompactionEventRef, V2CompactionPreview,
};
pub use compaction_shrink_sidecar::{
    TOOL_OUTPUT_PROJECTION_SIDECAR_PROJECTION_SCHEMA_VERSION,
    TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION, ToolOutputProjectionShrinkRecorded,
    ToolOutputProjectionSidecarProjection,
};
pub use compaction_sidecar::{
    COMPACTION_SIDECAR_PROJECTION_SCHEMA_VERSION, CONTINUATION_CHECKPOINT_V1_SCHEMA_VERSION,
    CompactionSidecarProjection, ContinuationCheckpointKind, ContinuationCheckpointV1,
    ContinuationEvidenceStatus, ContinuationItemAuthority, ContinuationItemOrigin,
    ContinuationItemPriority, ContinuationItemV1, ContinuationModelOutputItemV1,
    ContinuationModelOutputV1, ContinuationRedaction, ContinuationSnapshotScope,
    ContinuationSourceCatalog, ContinuationSourceRef, ContinuationTargetRequestFitV1,
    MAX_CONTINUATION_CHECKPOINT_ITEM_BYTES, MAX_CONTINUATION_CHECKPOINT_SECTION_ITEMS,
    ResolvedCompactionSidecar, TASK_MEMORY_RECORDED_V1_SCHEMA_VERSION, TaskMemoryInvalidatedEntry,
    TaskMemoryInvalidationReason, TaskMemoryRecordedV1,
};
pub use compaction_v2::{
    COMPACTION_LIFECYCLE_PROJECTION_SCHEMA_VERSION, CompactionAppliedV2, CompactionAttemptId,
    CompactionAttemptState, CompactionAttemptTerminal, CompactionCursor, CompactionFailureEntry,
    CompactionFailureReason, CompactionFallbackParent, CompactionId, CompactionInitiation,
    CompactionLifecycleProjection, CompactionStartedEntry,
};
pub use context_projection::{
    ContextTrustProjection, SESSION_CONTEXT_PROJECTION_SCHEMA_VERSION, SessionContextProjection,
    SessionProjectionEntry, TaskMemorySnapshotRelation,
};
pub use entry::*;
pub use facade::Session;
pub use portable_compaction::{
    PortableSemanticCompactionOutcome, PortableSemanticCompactionPreflight,
    PortableSemanticCompactionRequest, PortableTargetRequestMaterial,
};
pub(crate) use provider_attempt::ProviderPhysicalAttemptAudit;
pub use provider_attempt::{
    MAX_PROVIDER_PHYSICAL_ATTEMPT_OUTPUT_REFS, MAX_PROVIDER_PHYSICAL_ATTEMPT_REFERENCE_BYTES,
    MAX_PROVIDER_PHYSICAL_ATTEMPT_SIDE_EFFECT_REFS,
    PROVIDER_PHYSICAL_ATTEMPT_PROJECTION_SCHEMA_VERSION, PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
    ProviderNonGeneratingAttempt, ProviderNonGeneratingAttemptReceipt, ProviderPhysicalAttemptId,
    ProviderPhysicalAttemptOutcome, ProviderPhysicalAttemptProjection,
    ProviderPhysicalAttemptPurpose, ProviderPhysicalAttemptStartedEntry,
    ProviderPhysicalAttemptState, ProviderPhysicalAttemptTerminalEntry,
};
pub use provider_continuation::{
    MAX_PROVIDER_CONTINUATION_RESOLUTION_PROTECTED_REFS,
    MAX_PROVIDER_CONTINUATION_RESOLUTION_REFERENCE_BYTES,
    MAX_PROVIDER_CONTINUATION_RESOLUTION_RETAINED_REFS,
    MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_LEASE_MS,
    MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFERENCE_BYTES,
    MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFS, NativeProviderCompactionMetadata,
    PROVIDER_CONTINUATION_PROJECTION_SCHEMA_VERSION, PROVIDER_CONTINUATION_SCHEMA_VERSION,
    ProviderArtifactComposition, ProviderCompactionArtifactRef, ProviderContinuationActivationGate,
    ProviderContinuationAfterInputTokenCount, ProviderContinuationArtifactId,
    ProviderContinuationBeforeInputTokenCount, ProviderContinuationCandidate,
    ProviderContinuationCandidateId, ProviderContinuationCandidateInvalidatedEntry,
    ProviderContinuationCandidateInvalidationBasis,
    ProviderContinuationCandidateInvalidationReason,
    ProviderContinuationCandidateInvalidationState, ProviderContinuationCandidateRecordedEntry,
    ProviderContinuationCandidateState, ProviderContinuationEffectiveCompactionBudget,
    ProviderContinuationHandleRef, ProviderContinuationObservationId,
    ProviderContinuationObservationState, ProviderContinuationObservedEntry,
    ProviderContinuationPayloadId, ProviderContinuationPayloadIdentity,
    ProviderContinuationPayloadIntegrity, ProviderContinuationPayloadKind,
    ProviderContinuationPayloadLifecycleEntry, ProviderContinuationPayloadLifecycleState,
    ProviderContinuationPayloadSource, ProviderContinuationPayloadState,
    ProviderContinuationPayloadStorageRef, ProviderContinuationProjection,
    ProviderContinuationResolutionMode, ProviderContinuationRetentionPin,
    ProviderContinuationRetentionPinKind, ProviderContinuationSemanticCompressorIdentity,
    ProviderContinuationSemanticCompressorRequestFit, ProviderContinuationStateId,
    ProviderContinuationTargetExecutionIdentity, ProviderContinuationTargetTokenEvidence,
    ProviderContinuationToolClosureRecordedEntry, ProviderContinuationToolClosureState,
    ProviderObservedResolutionPlanId, ProviderObservedResolutionPlanLineage,
    ProviderObservedResolutionPlanRecordedEntry, ProviderObservedResolutionPlanState,
    ProviderToolCallClosureRef, provider_continuation_candidate_id_from_initiated,
    provider_continuation_candidate_id_from_observation,
    provider_continuation_candidate_invalidated_event_id,
    provider_continuation_candidate_recorded_event_id, provider_continuation_observation_id,
    provider_continuation_observed_event_id, provider_continuation_observed_payload_integrity_tag,
    provider_continuation_payload_id, provider_continuation_payload_lifecycle_event_id,
    provider_continuation_route_fingerprint, provider_continuation_tool_closure_recorded_event_id,
    provider_observed_resolution_plan_id, provider_observed_resolution_plan_recorded_event_id,
};
pub use provider_continuation_activation::{
    ProviderContinuationActivationEvaluator, ProviderContinuationActivationState,
};
pub use provider_continuation_admission::{
    ProviderObservedResolutionAdmission, ProviderObservedResolutionAdmissionEvaluator,
    ProviderObservedResolutionAdmissionRejection,
};
pub use provider_continuation_invalidation_coordinator::{
    ProviderContinuationCandidateInvalidationCoordinator,
    ProviderContinuationCandidateInvalidationPersistence,
};
pub use provider_continuation_payload_coordinator::{
    ProviderContinuationPayloadCommitResult, ProviderContinuationPayloadCoordinator,
    ProviderContinuationPayloadRecoveryReport, ProviderContinuationPayloadRetentionResult,
};
pub use provider_continuation_payload_store::{
    MAX_PROVIDER_CONTINUATION_PAYLOAD_BYTES, PROVIDER_CONTINUATION_SESSION_KEY_SLOT_ID,
    ProviderContinuationPayloadFinalizeResult, ProviderContinuationPayloadStageResult,
};
pub use provider_continuation_resolution_coordinator::{
    ProviderObservedResolutionPlanCoordinator, ProviderObservedResolutionPlanPersistence,
};
pub use provider_native_compaction::{
    NativeProviderCompactionAttempt, NativeProviderCompactionMaterialization,
    NativeProviderCompactionRequest,
};
pub use stats::session_stats_from_entries;
pub(crate) use store::session_entry_from_domain_event;
pub use store::{JsonlSessionStore, SessionStreamCompatibilityError};
pub use tool_output_projection::{
    MAX_TOOL_OUTPUT_PROJECTION_SHRINKS, ProjectedToolOutput,
    TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION, ToolOutputProjection, ToolOutputProjectionPolicy,
    ToolOutputProjectionShrink, ToolOutputProjectionSourceRef,
};
#[cfg(test)]
pub(crate) use writer::SessionWriterFault;
pub use writer::{
    DurableAppendExpectation, DurableAppendPermit, DurableAppendReceipt,
    DurableAppendRecordExpectation, DurableAppendRecordReceipt, DurableAuditBatch,
    DurableAuditError, DurableAuditRecord, DurableAuditWriter, DurableEventReconciliation,
    DurableEventReconciliationExpectation,
};

use context::*;
use projection::*;
use recovery::*;
use stats::*;
use store::*;

#[cfg(test)]
#[path = "tests/network_legacy_session_tests.rs"]
mod network_legacy_tests;
#[cfg(test)]
#[path = "tests/provider_native_compaction_tests.rs"]
mod provider_native_compaction_tests;
#[cfg(test)]
#[path = "tests/session_tests.rs"]
mod tests;
