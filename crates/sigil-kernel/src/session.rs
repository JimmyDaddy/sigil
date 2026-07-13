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
    CompactionConfig, ExternalProvenanceEntry, ExternalTrust, MemoryConfig, MemoryLoadReport,
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
        ConversationInputEditedEntry, ConversationInputQueueControlEntry,
        ConversationInputQueuedEntry, ConversationInputReorderedEntry,
        ConversationInputStatusEntry, ConversationQueueProjection,
    },
    event::{
        DomainEvent, DurableEventPayloadStorage, DurableEventType, EventClass, EventSyncClass,
        ProjectionApplyDecision, ProjectionCursor, StoredEvent, StoredEventDecode,
        TypedDomainEvent, TypedStoredEventDecode, decode_stored_event, decode_typed_stored_event,
        is_v2_stored_event_value, projection_apply_decision_for_record, stable_event_hash,
        stable_event_uuid,
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
        NetworkEffect, ToolAccess, ToolAccessWire, ToolError, ToolErrorKind, ToolPreviewSnapshot,
        ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
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

mod context;
mod entry;
mod facade;
mod projection;
mod recovery;
mod stats;
mod store;
mod writer;

pub use entry::*;
pub use facade::Session;
pub use stats::{latest_compaction_record, session_stats_from_entries};
pub use store::JsonlSessionStore;
pub(crate) use store::session_entry_from_domain_event;
#[cfg(test)]
pub(crate) use writer::SessionWriterFault;
pub use writer::{
    DurableAppendExpectation, DurableAppendPermit, DurableAppendReceipt,
    DurableAppendRecordExpectation, DurableAppendRecordReceipt, DurableAuditBatch,
    DurableAuditError, DurableAuditRecord, DurableAuditWriter,
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
#[path = "tests/session_tests.rs"]
mod tests;
