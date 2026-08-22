use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
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
        AgentApprovalRouteEntry, AgentDelegationAdmissionEntry, AgentElicitationRouteEntry,
        AgentMailboxMessageEntry, AgentMergeSafePointEntry, AgentProfileCapturedEntry,
        AgentProfilePolicyEntry, AgentProfilePolicyProjection, AgentProfileTrustEntry,
        AgentProfileTrustProjection, AgentResultContinuationEntry,
        AgentResultContinuationProjection, AgentRouteClosedEntry, AgentRunAttemptStartedEntry,
        AgentRunHeartbeatEntry, AgentRunInterruptedEntry, AgentThreadClosedEntry,
        AgentThreadDisplayNameEntry, AgentThreadMessageRoutedEntry,
        AgentThreadResultDeliveredEntry, AgentThreadResultRecordedEntry, AgentThreadStartedEntry,
        AgentThreadStateProjection, AgentThreadStatusChangedEntry, closed_agent_routes,
        interrupted_agent_attempts, interrupted_agent_mailbox_messages,
        interrupted_agent_result_continuations, interrupted_agent_threads,
        stale_expired_agent_approval_routes,
    },
    changeset::{ChangeSet, ChangeSetProjection, ChangeSetResult},
    context_engine::{
        ContextBodyRef, ContextInclusionReason, ContextItem, ContextItemId, ContextPackOptions,
        ContextSensitivity, ContextSource, ContextTrustLevel,
        DEFAULT_CONTEXT_RENDER_SNIPPET_MAX_BYTES, PackedContext, RUNTIME_CONTEXT_V2_HEADING,
        RUNTIME_CONTEXT_V2_NOTE, RUNTIME_CONTEXT_V2_PLACEMENT, RUNTIME_CONTEXT_V2_SCHEMA,
        RUNTIME_CONTEXT_V2_SELECTION_POLICY, RuntimeContextCandidates, SessionArchive,
        SessionArchiveEntry, context_provenance_row_v1, estimate_context_token_cost,
        pack_context_items, validate_context_render_snippet,
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
        PlanArtifactProjection, PlanDecisionRecordedEntry, PlanDraftCreatedEntry,
        PlanPermissionGrantedEntry, TaskCreatedFromPlanEntry,
    },
    plugin::{
        PluginHookExecutionFinishedEntry, PluginHookExecutionStartedEntry, PluginManifestSnapshot,
        PluginStateProjection, PluginTrustEntry,
    },
    provider::{
        CompletionRequest, MessageRole, ModelMessage, PREFIX_SNAPSHOT_CONTEXT_ITEM_LIMIT,
        PrefixRuntimeContextExclusionSummary, PrefixRuntimeContextItemSummary,
        PrefixRuntimeContextSummary, PrefixSnapshot, PrefixSnapshotMaterialization,
        ProviderContinuationState, ResponseHandle, SessionStats, UsageStats,
    },
    skill::{SkillIndexSnapshot, SkillLoadEntry, SkillStateProjection},
    task::{
        TaskChildSessionDisplayNameEntry, TaskChildSessionEntry, TaskFinalAnswerCommittedEntry,
        TaskParticipantAttemptEntry, TaskParticipantResultEntry,
        TaskParticipantRetryScheduledEntry, TaskPlanEntry, TaskRunCancellationScopeBoundEntry,
        TaskRunEntry, TaskStateProjection, TaskStepEntry, TaskSubagentApprovalRouteEntry,
        TaskSubagentElicitationRouteEntry, stale_task_approval_routes_for_restore,
    },
    task_handoff::{TaskHandoffProjection, TaskHandoffRequestedEntry, TaskHandoffResolvedEntry},
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
        IsolatedChangeSetProduced, IsolatedWorkspaceCleanupRecorded, IsolatedWorkspaceCreated,
        IsolatedWorkspacePrepared, MergeReviewRequested, MergeReviewResolved,
        WriteIsolationProjection, WriteLeaseAcquired, WriteLeaseReleased,
    },
};

static SESSION_LOG_IO_LOCK: Mutex<()> = Mutex::new(());
static SESSION_SHARED_LOCK_ATTEMPT_TOTAL: AtomicU64 = AtomicU64::new(0);
static SESSION_EXCLUSIVE_LOCK_ATTEMPT_TOTAL: AtomicU64 = AtomicU64::new(0);
static SESSION_LOCK_CONTENTION_TOTAL: AtomicU64 = AtomicU64::new(0);
static SESSION_LOCK_FAILURE_TOTAL: AtomicU64 = AtomicU64::new(0);
const SESSION_LOG_SHARED_LOCK_RETRIES: usize = 50;
const SESSION_LOG_SHARED_LOCK_RETRY_DELAY: Duration = Duration::from_millis(10);
const REQUEST_CONTEXT_V0_MAX_TOKENS: usize = 512;
const REQUEST_CONTEXT_V0_SESSION_ARCHIVE_LIMIT: usize = 4;
const REQUEST_CONTEXT_V0_EXTERNAL_SOURCE_LIMIT: usize = 64;
const REQUEST_CONTEXT_V0_ENTRY_MAX_BYTES: usize = 2048;
const REQUEST_CONTEXT_V0_ENTRY_OVERLAP_BYTES: usize = 256;
const UNSAFE_EXTERNAL_RECOVERY_AUDIT_REASON: &str =
    "recovery skipped unsafe external persistence control";

mod active_projection;
mod compaction_plan;
mod compaction_shrink_sidecar;
mod compaction_sidecar;
mod compaction_v2;
mod context;
mod context_projection;
mod continuity_v2;
mod conversation_promotion_projection;
mod conversation_queue_mutation;
mod conversation_queue_promotion;
mod effect_reconciliation;
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
mod provider_turn_recovery;
mod public_event_outbox;
mod recovery;
mod recovery_blocker;
mod stats;
mod store;
mod tool_artifact;
mod tool_output_pressure_projection;
mod tool_output_projection;
mod writer;

pub use active_projection::{
    ACTIVE_SESSION_PROJECTION_SCHEMA_VERSION, ActiveCompactionSummary, ActiveProjectionFamily,
    ActiveProjectionFrontier, ActiveProjectionMetricsSnapshot, ActiveProjectionNotice,
    ActiveProjectionObserver, ActiveProjectionSubscription, ActiveSessionProjectionSnapshot,
    ActiveTaskGuidanceState,
};
pub use compaction_plan::{
    ADAPTIVE_TAIL_SELECTION_SCHEMA_VERSION, AdaptiveTailPolicyV3, AdaptiveTailSelectionV3,
    COMPACTION_FOLD_PLAN_SCHEMA_VERSION, CompactionEventRef, CompactionFoldPlan,
    CompactionFoldProtectionReason, DEFAULT_TAIL_MAX_USABLE_CONTEXT_RATIO_PPM,
    DEFAULT_TAIL_MIN_COMPLETE_TURNS, DEFAULT_TAIL_RECENT_TURN_P95_MULTIPLIER_PPM,
    DEFAULT_TAIL_RECENT_TURN_SAMPLE_LIMIT, DEFAULT_TAIL_TARGET_MAX_TOKENS,
    DEFAULT_TAIL_TARGET_MIN_TOKENS, ProtectedCompactionEventRef, RetainedTurnGroupV3,
    TailTurnStateV3, V2CompactionPreview,
};
pub use compaction_shrink_sidecar::{
    TOOL_OUTPUT_CONTEXT_EPOCH_TRANSITION_SCHEMA_VERSION,
    TOOL_OUTPUT_PROJECTION_SIDECAR_PROJECTION_SCHEMA_VERSION,
    TOOL_OUTPUT_PROJECTION_SIDECAR_SCHEMA_VERSION, ToolOutputContextEpochTransitionReasonV1,
    ToolOutputContextEpochTransitionV1, ToolOutputProjectionShrinkRecorded,
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
    MAX_SEMANTIC_COMPACTION_OUTPUT_BYTES, MAX_SEMANTIC_COMPACTION_SOURCE_INDEX_ENTRIES,
    ResolvedCompactionSidecar, TASK_MEMORY_RECORDED_V1_SCHEMA_VERSION, TaskMemoryInvalidatedEntry,
    TaskMemoryInvalidationReason, TaskMemoryRecordedV1, build_semantic_compaction_instruction,
    parse_semantic_compaction_output,
};
pub use compaction_v2::{
    COMPACTION_LIFECYCLE_PROJECTION_SCHEMA_VERSION, CompactionAppliedV2, CompactionAttemptId,
    CompactionAttemptState, CompactionAttemptTerminal, CompactionCircuitBreakerDecisionV1,
    CompactionCircuitBreakerInputV1, CompactionCircuitScopeV1, CompactionCursor,
    CompactionEmergencyBlockingLayerV1, CompactionFailureEntry, CompactionFailureReason,
    CompactionFallbackParent, CompactionId, CompactionInitiation, CompactionLifecycleProjection,
    CompactionStartedEntry,
};
pub use context_projection::{
    ContextTrustProjection, SESSION_CONTEXT_PROJECTION_SCHEMA_VERSION, SessionContextProjection,
    SessionProjectionEntry, SessionProjectionOrigin, TaskMemorySnapshotRelation,
};
pub use continuity_v2::{
    ActiveConstraintV1, AnchoredStatementV1, CONVERSATION_CONTINUITY_V2_SCHEMA_VERSION,
    ConstraintStatusV1, ConversationContinuityV2, DurableArtifactRefV1, GroundedContinuityItemV2,
    ObjectiveAuthorityRefV1, SESSION_ANCHOR_V1_SCHEMA_VERSION, SessionAnchorRefV1, SessionAnchorV1,
    SourceSpanRefV1, UntrustedModelNarrativeV2,
};
pub use conversation_promotion_projection::conversation_transcript_entry_from_record;
pub use effect_reconciliation::{
    EFFECT_RECONCILIATION_SCHEMA_VERSION, EffectReconciliationOutcomeV1, EffectReconciliationProbe,
    EffectReconciliationProbeReceiptV1, EffectReconciliationProbeRequestV1,
    EffectReconciliationProbeStartedEntryV1, EffectReconciliationProjectionV1,
    EffectReconciliationRequiredEntryV1, EffectReconciliationTerminalEntryV1,
    ReconciliationProbeKindV1,
};
pub(crate) use effect_reconciliation::{
    validate_probe_started as validate_effect_reconciliation_probe_started,
    validate_required as validate_effect_reconciliation_required,
    validate_terminal as validate_effect_reconciliation_terminal,
};
pub use entry::*;
pub use facade::{Session, StableCompactionSnapshot};
pub use portable_compaction::{
    PortableSemanticCompactionOutcome, PortableSemanticCompactionPreflight,
    PortableSemanticCompactionRequest, PortableTargetRequestMaterial,
};
pub(crate) use provider_attempt::ProviderPhysicalAttemptAudit;
pub use provider_attempt::{
    MAX_PROVIDER_PHYSICAL_ATTEMPT_OUTPUT_REFS, MAX_PROVIDER_PHYSICAL_ATTEMPT_REFERENCE_BYTES,
    MAX_PROVIDER_PHYSICAL_ATTEMPT_SIDE_EFFECT_REFS, MAX_SEMANTIC_COMPACTION_GENERATION_BYTES,
    PROVIDER_PHYSICAL_ATTEMPT_PROJECTION_SCHEMA_VERSION, PROVIDER_PHYSICAL_ATTEMPT_SCHEMA_VERSION,
    ProviderNonGeneratingAttempt, ProviderNonGeneratingAttemptReceipt, ProviderPhysicalAttemptId,
    ProviderPhysicalAttemptOutcome, ProviderPhysicalAttemptProjection,
    ProviderPhysicalAttemptPurpose, ProviderPhysicalAttemptStartedEntry,
    ProviderPhysicalAttemptState, ProviderPhysicalAttemptTerminalEntry,
    SemanticCompactionGeneration, generate_semantic_compaction, new_provider_physical_attempt_id,
};
pub use provider_continuation::{
    MAX_PROVIDER_CONTINUATION_RESOLUTION_PROTECTED_REFS,
    MAX_PROVIDER_CONTINUATION_RESOLUTION_REFERENCE_BYTES,
    MAX_PROVIDER_CONTINUATION_RESOLUTION_RETAINED_REFS,
    MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_LEASE_MS,
    MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFERENCE_BYTES,
    MAX_PROVIDER_CONTINUATION_TOOL_CLOSURE_REFS, NATIVE_COMPACTION_CARRIER_SCHEMA_VERSION,
    NativeCarrierInvalidationV1, NativeCarrierPolicyV1, NativeCarrierResumeContextV1,
    NativeCarrierResumeDecisionV1, NativeCompactionCarrierV1, NativeProviderCompactionMetadata,
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
    ProviderRetentionPolicyV1, ProviderToolCallClosureRef,
    provider_continuation_candidate_id_from_initiated,
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
pub use provider_turn_recovery::{
    DEFAULT_PROVIDER_TURN_INITIAL_DELAY_MS, DEFAULT_PROVIDER_TURN_JITTER_RATIO_MILLIONTHS,
    DEFAULT_PROVIDER_TURN_MAX_CUMULATIVE_DELAY_MS, DEFAULT_PROVIDER_TURN_MAX_DELAY_MS,
    DEFAULT_PROVIDER_TURN_MAX_PARTIAL_OUTPUT_RETRIES, DEFAULT_PROVIDER_TURN_MAX_TRANSPORT_RETRIES,
    EffectSettlementStateV1, PROVIDER_TURN_RECOVERY_PROJECTION_SCHEMA_VERSION,
    PROVIDER_TURN_RECOVERY_SCHEMA_VERSION, ProviderOutputStateV1,
    ProviderTurnPartialOutputDiscardedEntryV1, ProviderTurnRecoveryEvidenceV1,
    ProviderTurnRecoveryExhaustedEntry, ProviderTurnRecoveryPolicyV1,
    ProviderTurnRecoveryProjection, ProviderTurnRecoveryRetryKindV1,
    ProviderTurnRecoveryScheduledEntry, ProviderTurnRecoveryStartedEntry,
    ProviderTurnRecoveryState, ProviderTurnRecoveryTerminalDispositionV1,
    ProviderTurnRecoveryTerminalError, ProviderTurnRequestMaterialAvailabilityV1,
    ProviderTurnTransportFallbackSelectedEntryV1, PublicProviderTurnPartialOutputDiscardedViewV1,
    PublicProviderTurnRecoveryActionV1, PublicProviderTurnRecoveryPhaseV1,
    PublicProviderTurnRecoveryViewV1, RecoveryBudgetProjectionV1, RecoveryDispositionV1,
};
pub(crate) use provider_turn_recovery::{
    ProviderTurnRecoveryAudit, validate_exhausted as validate_provider_turn_recovery_exhausted,
    validate_partial_output_discarded as validate_provider_turn_partial_output_discarded,
    validate_schedule as validate_provider_turn_recovery_schedule,
    validate_started as validate_provider_turn_recovery_started,
    validate_transport_fallback_selected as validate_provider_turn_transport_fallback_selected,
};
pub use public_event_outbox::{
    PUBLIC_EVENT_OUTBOX_SCHEMA_VERSION, PublicEventDeliveryReceiptV1, PublicEventOutboxEntryV1,
    PublicEventOutboxProjectionV1, PublicEventOutboxRecorder,
};
pub(crate) use public_event_outbox::{
    validate_delivery_receipt as validate_public_event_delivery_receipt,
    validate_outbox_entry as validate_public_event_outbox_entry,
};
pub use recovery_blocker::{
    RECOVERY_BLOCKER_PROJECTION_SCHEMA_VERSION, RecoveryBlockerProjectionV1,
    RecoveryBlockerResolutionStateV1,
};
pub use stats::session_stats_from_entries;
pub(crate) use store::session_entry_from_domain_event;
pub use store::{
    JsonlSessionStore, SessionIoBusyError, SessionIoBusyKind, SessionIoLockMetricsSnapshot,
    session_io_lock_metrics,
};
pub use tool_artifact::{
    ModelMessagePayloadV1, ProcessStreamCaptureConfigV1, ProviderToolResultMessageV1,
    TOOL_ARTIFACT_AVAILABILITY_CHANGED_SCHEMA_VERSION, TOOL_ARTIFACT_DESCRIPTOR_SCHEMA_VERSION,
    TOOL_ARTIFACT_MAX_BYTES, TOOL_ARTIFACT_ORPHAN_GRACE_MS, TOOL_ARTIFACT_READ_BYTES_PER_TURN,
    TOOL_ARTIFACT_READ_MAX_BYTES, TOOL_ARTIFACT_READ_MAX_LINES, TOOL_ARTIFACT_READ_SCHEMA_VERSION,
    TOOL_ARTIFACT_READS_PER_TURN, TOOL_ARTIFACT_SEARCH_MAX_CONTEXT_LINES,
    TOOL_ARTIFACT_SEARCH_MAX_MATCHES, TOOL_ARTIFACT_SESSION_BUDGET_BYTES,
    TOOL_ARTIFACT_TOMBSTONE_PLAN_SCHEMA_VERSION, TOOL_DISPLAY_VIEW_MAX_BYTES,
    TOOL_MODEL_VIEW_MAX_BYTES, TOOL_MODEL_VIEW_SCHEMA_VERSION, TOOL_RESULT_ERROR_SUMMARY_MAX_BYTES,
    TOOL_RESULT_EVENT_TARGET_BYTES, TOOL_RESULT_INLINE_CAPTURE_MAX_BYTES,
    TOOL_RESULT_RECORDED_SCHEMA_VERSION, ToolArtifactAvailability,
    ToolArtifactAvailabilityChangedV1, ToolArtifactAvailabilityReasonV1,
    ToolArtifactAvailabilityStateV1, ToolArtifactBindingV1, ToolArtifactBudgetedReadV1,
    ToolArtifactCaptureSink, ToolArtifactCompleteness, ToolArtifactDescriptorV1,
    ToolArtifactEncoding, ToolArtifactGcReportV1, ToolArtifactGcRootsV1, ToolArtifactId,
    ToolArtifactManifestEntryV1, ToolArtifactPageEncoding, ToolArtifactPageV1,
    ToolArtifactReadBudgetV1, ToolArtifactReadOutcome, ToolArtifactReadRecordedV1,
    ToolArtifactRefV1, ToolArtifactRetentionClass, ToolArtifactRetrievalPolicyV1,
    ToolArtifactSelectorV1, ToolArtifactSensitivity, ToolArtifactStore,
    ToolArtifactTombstonePlannedV1, ToolArtifactTrashPruneReportV1, ToolArtifactTruncationV1,
    ToolArtifactUnavailableV1, ToolDisplayCapability, ToolDisplayViewV1, ToolErrorSummaryV1,
    ToolExecutionCapturePlanV1, ToolModelViewV1, ToolOutputPersistencePolicy, ToolOutputSegmentV1,
    ToolOutputStreamLayoutV1, ToolOutputStreamV1, ToolPolicyCompletenessV1, ToolPreviewKind,
    ToolPreviewTruncationReasonV1, ToolResultCaptureCompletenessV1, ToolResultCapturePathV1,
    ToolResultCaptureTelemetryV1, ToolResultFactsV1, ToolResultFailureStageV1, ToolResultOutcomeV1,
    ToolResultRecordedV2, ToolResultRecordedV3, ToolResultTerminalFallbackV1, ToolResultViewsV2,
    ToolResultWireSemanticsV1, ToolSourceCompletenessV1, ToolStorageCompletenessV1,
};
pub use tool_artifact::{
    TOOL_MODEL_VIEW_BATCH_BUDGET_BYTES, TOOL_MODEL_VIEW_BATCH_FLOOR_BYTES,
    TOOL_MODEL_VIEW_BATCH_MAX_RESULTS, allocate_batch_preview_limits,
    tool_model_view_initial_limit,
};
pub use tool_output_pressure_projection::{
    TOOL_OUTPUT_AGED_RESULT_MAX_BYTES, TOOL_OUTPUT_AGED_RESULT_TARGET_TOKENS,
    TOOL_OUTPUT_AGING_ACTIVATION_SCHEMA_VERSION, TOOL_OUTPUT_AGING_MAX_RESULTS,
    TOOL_OUTPUT_AGING_POLICY_VERSION, TOOL_OUTPUT_ARCHIVED_ARTIFACT_BINDING_MAX,
    TOOL_OUTPUT_MIN_BATCH_RECLAIM_TOKENS, TOOL_OUTPUT_OPEN_CALL_MAX,
    TOOL_OUTPUT_PRESSURE_HARD_MAX_RESULTS, TOOL_OUTPUT_PRESSURE_MAX_RESULTS,
    TOOL_OUTPUT_PRESSURE_PROJECTION_SCHEMA_VERSION, TOOL_OUTPUT_RECENT_PROTECTED_TOKENS,
    ToolOutputAgedViewV1, ToolOutputAgingActivatedV1, ToolOutputAgingBatchV1,
    ToolOutputAgingProjectionV1, ToolOutputAgingReasonV1, ToolOutputArchivedArtifactBindingV1,
    ToolOutputPressureItemV1, ToolOutputPressureProjectionV1, ToolOutputPressureSnapshotV1,
    ToolOutputRetentionClassV1,
};
pub use tool_output_projection::{
    MAX_TOOL_OUTPUT_PROJECTION_SHRINKS, ProjectedToolOutput,
    RECOVERABLE_TOOL_OUTPUT_SHRINK_CANDIDATE_SCHEMA_VERSION,
    RecoverableToolOutputShrinkCandidateV1, TOOL_OUTPUT_PROJECTION_SHRINK_SCHEMA_VERSION,
    ToolOutputArtifactRefV1, ToolOutputProjection, ToolOutputProjectionPolicy,
    ToolOutputProjectionShrink, ToolOutputProjectionSourceRef, ToolOutputShrinkReasonV1,
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
pub(crate) fn append_current_test_session_identity(store: &JsonlSessionStore) -> Result<()> {
    store.append(&SessionLogEntry::Control(ControlEntry::SessionIdentity {
        provider_name: "test".to_owned(),
        model_name: "model".to_owned(),
        resolved_model_route: None,
    }))
}

#[cfg(test)]
#[path = "tests/provider_native_compaction_tests.rs"]
mod provider_native_compaction_tests;
#[cfg(test)]
#[path = "session/tests/recovery_blocker_tests.rs"]
mod recovery_blocker_tests;
#[cfg(test)]
#[path = "tests/session_tests.rs"]
mod tests;
