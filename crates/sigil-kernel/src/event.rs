use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ChangeSet, ChangeSetResult, ControlEntry, JobIntentEntry, ModelMessage, MutationCommitted,
    MutationPrepared, PathTrustZone, PermissionConfirmation, PermissionRisk,
    ProviderContinuationState, SessionLogEntry, StepLeaseEntry, StepLeaseHeartbeatEntry,
    TerminalTaskEntry, ToolCall, ToolOperation, ToolPreview, ToolProgressEvent, ToolResult,
    ToolSpec, ToolSubject, UsageStats, VerificationCheckRunEntry, VerificationRecordedEntry,
    WorkspaceMutationDetected,
};

/// Current schema version for public run events consumed by external adapters.
pub const PUBLIC_RUN_EVENT_SCHEMA_VERSION: u32 = 1;

/// Current schema version for durable stored event envelopes.
pub const STORED_EVENT_SCHEMA_VERSION: u16 = 1;

/// Checksum prefix for deterministic stored event records.
pub const RECORD_CHECKSUM_PREFIX: &str = "sha256:jcs-v1:";

/// Conservative first-pass event byte limit for one JSONL record.
pub const MAX_EVENT_BYTES: usize = 1024 * 1024;

/// Conservative first-pass nesting limit for stored event payloads.
pub const MAX_PAYLOAD_DEPTH: usize = 64;

pub type EventId = String;
pub type SessionId = String;

/// Durable event criticality used by older readers to decide fail-open vs fail-closed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventClass {
    Critical,
    NonCritical,
}

/// Initial durable append sync classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSyncClass {
    NormalEvent,
    RecoveryCritical,
    TailRecovery,
}

/// Known durable event type names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DurableEventType {
    UserMessageRecorded,
    AssistantMessageRecorded,
    ToolResultRecorded,
    SessionEntryRecorded,
    RunStatusChanged,
    RunFinalized,
    ToolExecutionStarted,
    ToolExecutionFinished,
    ApprovalResolved,
    PlanDraftCreated,
    PlanDecisionRecorded,
    PlanPermissionGranted,
    TaskCreatedFromPlan,
    MutationPrepared,
    MutationCommitted,
    MutationReconciled,
    MutationBatchStarted,
    MutationBatchFinished,
    WriteCommitted,
    WorkspaceMutationDetected,
    CheckpointRestored,
    MutationArtifactCleanupRequested,
    MutationArtifactLifecycleRecorded,
    CommandFinished,
    CheckFinished,
    CheckSpecRecorded,
    DiagnosticRecorded,
    TodoChanged,
    VerificationRecorded,
    VerificationPolicyChanged,
    VerificationCheckRun,
    EnvironmentFingerprintRecorded,
    ReadinessEvaluated,
    TaskStatusChanged,
    ChildVerificationReceiptLinked,
    ChildChangesetMerged,
    AgentMergeApplied,
    WriteLeaseAcquired,
    WriteLeaseReleased,
    IsolatedWorkspaceCreated,
    IsolatedChangeSetProduced,
    MergeReviewRequested,
    MergeReviewResolved,
    JobIntentRecorded,
    StepLeaseRecorded,
    StepLeaseHeartbeatRecorded,
    WorkspaceTrustDecision,
    ContextSourceCaptured,
    EgressDecisionRecorded,
    ExtensionTrustDecision,
    PluginHookExecutionStarted,
    PluginHookExecutionFinished,
    SandboxDecisionRecorded,
    LogTailRecovered,
    Legacy,
}

impl DurableEventType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserMessageRecorded => "user_message_recorded",
            Self::AssistantMessageRecorded => "assistant_message_recorded",
            Self::ToolResultRecorded => "tool_result_recorded",
            Self::SessionEntryRecorded => "session_entry_recorded",
            Self::RunStatusChanged => "run_status_changed",
            Self::RunFinalized => "run_finalized",
            Self::ToolExecutionStarted => "tool_execution_started",
            Self::ToolExecutionFinished => "tool_execution_finished",
            Self::ApprovalResolved => "approval_resolved",
            Self::PlanDraftCreated => "plan_draft_created",
            Self::PlanDecisionRecorded => "plan_decision_recorded",
            Self::PlanPermissionGranted => "plan_permission_granted",
            Self::TaskCreatedFromPlan => "task_created_from_plan",
            Self::MutationPrepared => "mutation_prepared",
            Self::MutationCommitted => "mutation_committed",
            Self::MutationReconciled => "mutation_reconciled",
            Self::MutationBatchStarted => "mutation_batch_started",
            Self::MutationBatchFinished => "mutation_batch_finished",
            Self::WriteCommitted => "write_committed",
            Self::WorkspaceMutationDetected => "workspace_mutation_detected",
            Self::CheckpointRestored => "checkpoint_restored",
            Self::MutationArtifactCleanupRequested => "mutation_artifact_cleanup_requested",
            Self::MutationArtifactLifecycleRecorded => "mutation_artifact_lifecycle_recorded",
            Self::CommandFinished => "command_finished",
            Self::CheckFinished => "check_finished",
            Self::CheckSpecRecorded => "check_spec_recorded",
            Self::DiagnosticRecorded => "diagnostic_recorded",
            Self::TodoChanged => "todo_changed",
            Self::VerificationRecorded => "verification_recorded",
            Self::VerificationPolicyChanged => "verification_policy_changed",
            Self::VerificationCheckRun => "verification_check_run",
            Self::EnvironmentFingerprintRecorded => "environment_fingerprint_recorded",
            Self::ReadinessEvaluated => "readiness_evaluated",
            Self::TaskStatusChanged => "task_status_changed",
            Self::ChildVerificationReceiptLinked => "child_verification_receipt_linked",
            Self::ChildChangesetMerged => "child_changeset_merged",
            Self::AgentMergeApplied => "agent_merge_applied",
            Self::WriteLeaseAcquired => "write_lease_acquired",
            Self::WriteLeaseReleased => "write_lease_released",
            Self::IsolatedWorkspaceCreated => "isolated_workspace_created",
            Self::IsolatedChangeSetProduced => "isolated_changeset_produced",
            Self::MergeReviewRequested => "merge_review_requested",
            Self::MergeReviewResolved => "merge_review_resolved",
            Self::JobIntentRecorded => "job_intent_recorded",
            Self::StepLeaseRecorded => "step_lease_recorded",
            Self::StepLeaseHeartbeatRecorded => "step_lease_heartbeat_recorded",
            Self::WorkspaceTrustDecision => "workspace_trust_decision",
            Self::ContextSourceCaptured => "context_source_captured",
            Self::EgressDecisionRecorded => "egress_decision_recorded",
            Self::ExtensionTrustDecision => "extension_trust_decision",
            Self::PluginHookExecutionStarted => "plugin_hook_execution_started",
            Self::PluginHookExecutionFinished => "plugin_hook_execution_finished",
            Self::SandboxDecisionRecorded => "sandbox_decision_recorded",
            Self::LogTailRecovered => "log_tail_recovered",
            Self::Legacy => "legacy",
        }
    }

    pub fn from_event_type(value: &str) -> Option<Self> {
        Some(match value {
            "user_message_recorded" => Self::UserMessageRecorded,
            "assistant_message_recorded" => Self::AssistantMessageRecorded,
            "tool_result_recorded" => Self::ToolResultRecorded,
            "session_entry_recorded" => Self::SessionEntryRecorded,
            "run_status_changed" => Self::RunStatusChanged,
            "run_finalized" => Self::RunFinalized,
            "tool_execution_started" => Self::ToolExecutionStarted,
            "tool_execution_finished" => Self::ToolExecutionFinished,
            "approval_resolved" => Self::ApprovalResolved,
            "plan_draft_created" => Self::PlanDraftCreated,
            "plan_decision_recorded" => Self::PlanDecisionRecorded,
            "plan_permission_granted" => Self::PlanPermissionGranted,
            "task_created_from_plan" => Self::TaskCreatedFromPlan,
            "mutation_prepared" => Self::MutationPrepared,
            "mutation_committed" => Self::MutationCommitted,
            "mutation_reconciled" => Self::MutationReconciled,
            "mutation_batch_started" => Self::MutationBatchStarted,
            "mutation_batch_finished" => Self::MutationBatchFinished,
            "write_committed" => Self::WriteCommitted,
            "workspace_mutation_detected" => Self::WorkspaceMutationDetected,
            "checkpoint_restored" => Self::CheckpointRestored,
            "mutation_artifact_cleanup_requested" => Self::MutationArtifactCleanupRequested,
            "mutation_artifact_lifecycle_recorded" => Self::MutationArtifactLifecycleRecorded,
            "command_finished" => Self::CommandFinished,
            "check_finished" => Self::CheckFinished,
            "check_spec_recorded" => Self::CheckSpecRecorded,
            "diagnostic_recorded" => Self::DiagnosticRecorded,
            "todo_changed" => Self::TodoChanged,
            "verification_recorded" => Self::VerificationRecorded,
            "verification_policy_changed" => Self::VerificationPolicyChanged,
            "verification_check_run" => Self::VerificationCheckRun,
            "environment_fingerprint_recorded" => Self::EnvironmentFingerprintRecorded,
            "readiness_evaluated" => Self::ReadinessEvaluated,
            "task_status_changed" => Self::TaskStatusChanged,
            "child_verification_receipt_linked" => Self::ChildVerificationReceiptLinked,
            "child_changeset_merged" => Self::ChildChangesetMerged,
            "agent_merge_applied" => Self::AgentMergeApplied,
            "write_lease_acquired" => Self::WriteLeaseAcquired,
            "write_lease_released" => Self::WriteLeaseReleased,
            "isolated_workspace_created" => Self::IsolatedWorkspaceCreated,
            "isolated_changeset_produced" => Self::IsolatedChangeSetProduced,
            "merge_review_requested" => Self::MergeReviewRequested,
            "merge_review_resolved" => Self::MergeReviewResolved,
            "job_intent_recorded" => Self::JobIntentRecorded,
            "step_lease_recorded" => Self::StepLeaseRecorded,
            "step_lease_heartbeat_recorded" => Self::StepLeaseHeartbeatRecorded,
            "workspace_trust_decision" => Self::WorkspaceTrustDecision,
            "context_source_captured" => Self::ContextSourceCaptured,
            "egress_decision_recorded" => Self::EgressDecisionRecorded,
            "extension_trust_decision" => Self::ExtensionTrustDecision,
            "plugin_hook_execution_started" => Self::PluginHookExecutionStarted,
            "plugin_hook_execution_finished" => Self::PluginHookExecutionFinished,
            "sandbox_decision_recorded" => Self::SandboxDecisionRecorded,
            "log_tail_recovered" => Self::LogTailRecovered,
            "legacy" => Self::Legacy,
            _ => return None,
        })
    }

    pub fn sync_class(self) -> Option<EventSyncClass> {
        if self == Self::Legacy {
            return None;
        }
        if self == Self::LogTailRecovered {
            return Some(EventSyncClass::TailRecovery);
        }
        if matches!(
            self,
            Self::UserMessageRecorded
                | Self::AssistantMessageRecorded
                | Self::ContextSourceCaptured
        ) {
            return Some(EventSyncClass::NormalEvent);
        }
        Some(EventSyncClass::RecoveryCritical)
    }

    pub fn expected_event_class(self) -> Option<EventClass> {
        if self == Self::Legacy {
            return None;
        }
        if matches!(
            self,
            Self::ContextSourceCaptured | Self::SessionEntryRecorded
        ) {
            return Some(EventClass::NonCritical);
        }
        Some(EventClass::Critical)
    }

    pub fn appendable(self) -> bool {
        !matches!(self, Self::Legacy)
    }
}

/// Ordered known durable event types.
pub const ALL_DURABLE_EVENT_TYPES: &[DurableEventType] = &[
    DurableEventType::UserMessageRecorded,
    DurableEventType::AssistantMessageRecorded,
    DurableEventType::ToolResultRecorded,
    DurableEventType::SessionEntryRecorded,
    DurableEventType::RunStatusChanged,
    DurableEventType::RunFinalized,
    DurableEventType::ToolExecutionStarted,
    DurableEventType::ToolExecutionFinished,
    DurableEventType::ApprovalResolved,
    DurableEventType::PlanDraftCreated,
    DurableEventType::PlanDecisionRecorded,
    DurableEventType::PlanPermissionGranted,
    DurableEventType::TaskCreatedFromPlan,
    DurableEventType::MutationPrepared,
    DurableEventType::MutationCommitted,
    DurableEventType::MutationReconciled,
    DurableEventType::MutationBatchStarted,
    DurableEventType::MutationBatchFinished,
    DurableEventType::WriteCommitted,
    DurableEventType::WorkspaceMutationDetected,
    DurableEventType::CheckpointRestored,
    DurableEventType::MutationArtifactCleanupRequested,
    DurableEventType::MutationArtifactLifecycleRecorded,
    DurableEventType::CommandFinished,
    DurableEventType::CheckFinished,
    DurableEventType::CheckSpecRecorded,
    DurableEventType::DiagnosticRecorded,
    DurableEventType::TodoChanged,
    DurableEventType::VerificationRecorded,
    DurableEventType::VerificationPolicyChanged,
    DurableEventType::VerificationCheckRun,
    DurableEventType::EnvironmentFingerprintRecorded,
    DurableEventType::ReadinessEvaluated,
    DurableEventType::TaskStatusChanged,
    DurableEventType::ChildVerificationReceiptLinked,
    DurableEventType::ChildChangesetMerged,
    DurableEventType::AgentMergeApplied,
    DurableEventType::WriteLeaseAcquired,
    DurableEventType::WriteLeaseReleased,
    DurableEventType::IsolatedWorkspaceCreated,
    DurableEventType::IsolatedChangeSetProduced,
    DurableEventType::MergeReviewRequested,
    DurableEventType::MergeReviewResolved,
    DurableEventType::JobIntentRecorded,
    DurableEventType::StepLeaseRecorded,
    DurableEventType::StepLeaseHeartbeatRecorded,
    DurableEventType::WorkspaceTrustDecision,
    DurableEventType::ContextSourceCaptured,
    DurableEventType::EgressDecisionRecorded,
    DurableEventType::ExtensionTrustDecision,
    DurableEventType::PluginHookExecutionStarted,
    DurableEventType::PluginHookExecutionFinished,
    DurableEventType::SandboxDecisionRecorded,
    DurableEventType::LogTailRecovered,
    DurableEventType::Legacy,
];

/// v2 durable event envelope persisted to JSONL.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub struct StoredEvent {
    pub schema_version: u16,
    pub event_type: String,
    pub event_version: u16,
    pub event_class: EventClass,
    pub event_id: EventId,
    pub session_id: SessionId,
    pub stream_sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurred_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub causation_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<SessionId>,
    pub record_checksum: String,
    pub payload: Value,
}

impl StoredEvent {
    pub fn new(
        event_type: DurableEventType,
        event_class: EventClass,
        event_id: EventId,
        session_id: SessionId,
        stream_sequence: u64,
        payload: Value,
    ) -> Result<Self> {
        let expected_class = event_type.expected_event_class().ok_or_else(|| {
            anyhow::anyhow!("{} cannot be appended as a v2 event", event_type.as_str())
        })?;
        if event_class != expected_class {
            bail!(
                "{} event_class must be {:?}, got {:?}",
                event_type.as_str(),
                expected_class,
                event_class
            );
        }
        Self::new_raw(
            event_type.as_str(),
            event_class,
            event_id,
            session_id,
            stream_sequence,
            payload,
        )
    }

    pub fn new_raw(
        event_type: impl Into<String>,
        event_class: EventClass,
        event_id: EventId,
        session_id: SessionId,
        stream_sequence: u64,
        payload: Value,
    ) -> Result<Self> {
        ensure_payload_depth(&payload)?;
        let mut event = Self {
            schema_version: STORED_EVENT_SCHEMA_VERSION,
            event_type: event_type.into(),
            event_version: 1,
            event_class,
            event_id,
            session_id,
            stream_sequence,
            occurred_at: None,
            correlation_id: None,
            causation_id: None,
            parent_session_id: None,
            record_checksum: String::new(),
            payload,
        };
        event.record_checksum = event.compute_record_checksum()?;
        event.ensure_size_limit()?;
        Ok(event)
    }

    pub fn event_kind(&self) -> Option<DurableEventType> {
        DurableEventType::from_event_type(&self.event_type)
    }

    pub fn sync_class(&self) -> Result<EventSyncClass> {
        let Some(kind) = self.event_kind() else {
            return match self.event_class {
                EventClass::NonCritical => Ok(EventSyncClass::NormalEvent),
                EventClass::Critical => bail!("unknown critical event {}", self.event_type),
            };
        };
        kind.sync_class()
            .ok_or_else(|| anyhow::anyhow!("{} cannot be appended as a v2 event", self.event_type))
    }

    pub fn from_json_str(line: &str) -> Result<Self> {
        let value: Value =
            serde_json::from_str(line).context("failed to parse stored event json")?;
        let event = Self::from_value(value)?;
        event.verify_record_checksum()?;
        Ok(event)
    }

    pub fn from_value(value: Value) -> Result<Self> {
        if value.get("event_type").and_then(Value::as_str).is_none() {
            bail!("stored event is missing event_type");
        }
        if value.get("event_class").and_then(Value::as_str).is_none() {
            bail!("stored event is missing trusted event_class");
        }
        let event: Self =
            serde_json::from_value(value).context("failed to deserialize stored event envelope")?;
        if event.schema_version != STORED_EVENT_SCHEMA_VERSION {
            bail!(
                "unsupported stored event schema_version {}",
                event.schema_version
            );
        }
        if let Some(kind) = event.event_kind() {
            if event.event_version != 1 {
                bail!(
                    "unsupported {} event_version {}",
                    kind.as_str(),
                    event.event_version
                );
            }
        } else if event.event_class == EventClass::Critical {
            bail!("unknown critical event {}", event.event_type);
        }
        ensure_payload_depth(&event.payload)?;
        event.ensure_size_limit()?;
        Ok(event)
    }

    pub fn compute_record_checksum(&self) -> Result<String> {
        let body = self.checksum_body_value();
        let bytes = canonical_json_bytes(&body)?;
        let digest = Sha256::digest(&bytes);
        Ok(format!("{RECORD_CHECKSUM_PREFIX}{digest:x}"))
    }

    pub fn verify_record_checksum(&self) -> Result<()> {
        let expected = self.compute_record_checksum()?;
        if self.record_checksum != expected {
            bail!("stored event checksum mismatch");
        }
        Ok(())
    }

    pub fn to_json_line(&self) -> Result<String> {
        self.verify_record_checksum()?;
        self.ensure_size_limit()?;
        let mut line = serde_json::to_string(self).context("failed to serialize stored event")?;
        line.push('\n');
        Ok(line)
    }

    fn checksum_body_value(&self) -> Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "event_type": self.event_type,
            "event_version": self.event_version,
            "event_class": self.event_class,
            "event_id": self.event_id,
            "session_id": self.session_id,
            "stream_sequence": self.stream_sequence,
            "occurred_at": self.occurred_at,
            "correlation_id": self.correlation_id,
            "causation_id": self.causation_id,
            "parent_session_id": self.parent_session_id,
            "payload": self.payload,
        })
    }

    fn ensure_size_limit(&self) -> Result<()> {
        let bytes = serde_json::to_vec(self).context("failed to size stored event")?;
        if bytes.len() > MAX_EVENT_BYTES {
            bail!("stored event exceeds maximum byte size");
        }
        Ok(())
    }
}

/// Versioned payload carried by a reducer-facing domain event.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainPayload {
    pub event_version: u16,
    pub payload: Value,
}

/// Strong reducer-facing event.
#[derive(Debug, Clone, PartialEq)]
pub enum DurableDomainEvent {
    UserMessageRecorded(DomainPayload),
    AssistantMessageRecorded(DomainPayload),
    ToolResultRecorded(DomainPayload),
    SessionEntryRecorded(DomainPayload),
    RunStatusChanged(DomainPayload),
    RunFinalized(DomainPayload),
    ToolExecutionStarted(DomainPayload),
    ToolExecutionFinished(DomainPayload),
    ApprovalResolved(DomainPayload),
    PlanDraftCreated(DomainPayload),
    PlanDecisionRecorded(DomainPayload),
    PlanPermissionGranted(DomainPayload),
    TaskCreatedFromPlan(DomainPayload),
    MutationPrepared(DomainPayload),
    MutationCommitted(DomainPayload),
    MutationReconciled(DomainPayload),
    MutationBatchStarted(DomainPayload),
    MutationBatchFinished(DomainPayload),
    WriteCommitted(DomainPayload),
    WorkspaceMutationDetected(DomainPayload),
    CheckpointRestored(DomainPayload),
    MutationArtifactCleanupRequested(DomainPayload),
    MutationArtifactLifecycleRecorded(DomainPayload),
    CommandFinished(DomainPayload),
    CheckFinished(DomainPayload),
    CheckSpecRecorded(DomainPayload),
    DiagnosticRecorded(DomainPayload),
    TodoChanged(DomainPayload),
    VerificationRecorded(DomainPayload),
    VerificationPolicyChanged(DomainPayload),
    VerificationCheckRun(DomainPayload),
    EnvironmentFingerprintRecorded(DomainPayload),
    ReadinessEvaluated(DomainPayload),
    TaskStatusChanged(DomainPayload),
    ChildVerificationReceiptLinked(DomainPayload),
    ChildChangesetMerged(DomainPayload),
    AgentMergeApplied(DomainPayload),
    WriteLeaseAcquired(DomainPayload),
    WriteLeaseReleased(DomainPayload),
    IsolatedWorkspaceCreated(DomainPayload),
    IsolatedChangeSetProduced(DomainPayload),
    MergeReviewRequested(DomainPayload),
    MergeReviewResolved(DomainPayload),
    JobIntentRecorded(DomainPayload),
    StepLeaseRecorded(DomainPayload),
    StepLeaseHeartbeatRecorded(DomainPayload),
    WorkspaceTrustDecision(DomainPayload),
    ContextSourceCaptured(DomainPayload),
    EgressDecisionRecorded(DomainPayload),
    ExtensionTrustDecision(DomainPayload),
    PluginHookExecutionStarted(DomainPayload),
    PluginHookExecutionFinished(DomainPayload),
    SandboxDecisionRecorded(DomainPayload),
    LogTailRecovered(DomainPayload),
    Legacy(LegacyEvent),
}

impl DurableDomainEvent {
    pub fn event_type(&self) -> DurableEventType {
        match self {
            Self::UserMessageRecorded(_) => DurableEventType::UserMessageRecorded,
            Self::AssistantMessageRecorded(_) => DurableEventType::AssistantMessageRecorded,
            Self::ToolResultRecorded(_) => DurableEventType::ToolResultRecorded,
            Self::SessionEntryRecorded(_) => DurableEventType::SessionEntryRecorded,
            Self::RunStatusChanged(_) => DurableEventType::RunStatusChanged,
            Self::RunFinalized(_) => DurableEventType::RunFinalized,
            Self::ToolExecutionStarted(_) => DurableEventType::ToolExecutionStarted,
            Self::ToolExecutionFinished(_) => DurableEventType::ToolExecutionFinished,
            Self::ApprovalResolved(_) => DurableEventType::ApprovalResolved,
            Self::PlanDraftCreated(_) => DurableEventType::PlanDraftCreated,
            Self::PlanDecisionRecorded(_) => DurableEventType::PlanDecisionRecorded,
            Self::PlanPermissionGranted(_) => DurableEventType::PlanPermissionGranted,
            Self::TaskCreatedFromPlan(_) => DurableEventType::TaskCreatedFromPlan,
            Self::MutationPrepared(_) => DurableEventType::MutationPrepared,
            Self::MutationCommitted(_) => DurableEventType::MutationCommitted,
            Self::MutationReconciled(_) => DurableEventType::MutationReconciled,
            Self::MutationBatchStarted(_) => DurableEventType::MutationBatchStarted,
            Self::MutationBatchFinished(_) => DurableEventType::MutationBatchFinished,
            Self::WriteCommitted(_) => DurableEventType::WriteCommitted,
            Self::WorkspaceMutationDetected(_) => DurableEventType::WorkspaceMutationDetected,
            Self::CheckpointRestored(_) => DurableEventType::CheckpointRestored,
            Self::MutationArtifactCleanupRequested(_) => {
                DurableEventType::MutationArtifactCleanupRequested
            }
            Self::MutationArtifactLifecycleRecorded(_) => {
                DurableEventType::MutationArtifactLifecycleRecorded
            }
            Self::CommandFinished(_) => DurableEventType::CommandFinished,
            Self::CheckFinished(_) => DurableEventType::CheckFinished,
            Self::CheckSpecRecorded(_) => DurableEventType::CheckSpecRecorded,
            Self::DiagnosticRecorded(_) => DurableEventType::DiagnosticRecorded,
            Self::TodoChanged(_) => DurableEventType::TodoChanged,
            Self::VerificationRecorded(_) => DurableEventType::VerificationRecorded,
            Self::VerificationPolicyChanged(_) => DurableEventType::VerificationPolicyChanged,
            Self::VerificationCheckRun(_) => DurableEventType::VerificationCheckRun,
            Self::EnvironmentFingerprintRecorded(_) => {
                DurableEventType::EnvironmentFingerprintRecorded
            }
            Self::ReadinessEvaluated(_) => DurableEventType::ReadinessEvaluated,
            Self::TaskStatusChanged(_) => DurableEventType::TaskStatusChanged,
            Self::ChildVerificationReceiptLinked(_) => {
                DurableEventType::ChildVerificationReceiptLinked
            }
            Self::ChildChangesetMerged(_) => DurableEventType::ChildChangesetMerged,
            Self::AgentMergeApplied(_) => DurableEventType::AgentMergeApplied,
            Self::WriteLeaseAcquired(_) => DurableEventType::WriteLeaseAcquired,
            Self::WriteLeaseReleased(_) => DurableEventType::WriteLeaseReleased,
            Self::IsolatedWorkspaceCreated(_) => DurableEventType::IsolatedWorkspaceCreated,
            Self::IsolatedChangeSetProduced(_) => DurableEventType::IsolatedChangeSetProduced,
            Self::MergeReviewRequested(_) => DurableEventType::MergeReviewRequested,
            Self::MergeReviewResolved(_) => DurableEventType::MergeReviewResolved,
            Self::JobIntentRecorded(_) => DurableEventType::JobIntentRecorded,
            Self::StepLeaseRecorded(_) => DurableEventType::StepLeaseRecorded,
            Self::StepLeaseHeartbeatRecorded(_) => DurableEventType::StepLeaseHeartbeatRecorded,
            Self::WorkspaceTrustDecision(_) => DurableEventType::WorkspaceTrustDecision,
            Self::ContextSourceCaptured(_) => DurableEventType::ContextSourceCaptured,
            Self::EgressDecisionRecorded(_) => DurableEventType::EgressDecisionRecorded,
            Self::ExtensionTrustDecision(_) => DurableEventType::ExtensionTrustDecision,
            Self::PluginHookExecutionStarted(_) => DurableEventType::PluginHookExecutionStarted,
            Self::PluginHookExecutionFinished(_) => DurableEventType::PluginHookExecutionFinished,
            Self::SandboxDecisionRecorded(_) => DurableEventType::SandboxDecisionRecorded,
            Self::LogTailRecovered(_) => DurableEventType::LogTailRecovered,
            Self::Legacy(_) => DurableEventType::Legacy,
        }
    }

    pub fn payload(&self) -> Option<&DomainPayload> {
        match self {
            Self::UserMessageRecorded(payload)
            | Self::AssistantMessageRecorded(payload)
            | Self::ToolResultRecorded(payload)
            | Self::SessionEntryRecorded(payload)
            | Self::RunStatusChanged(payload)
            | Self::RunFinalized(payload)
            | Self::ToolExecutionStarted(payload)
            | Self::ToolExecutionFinished(payload)
            | Self::ApprovalResolved(payload)
            | Self::PlanDraftCreated(payload)
            | Self::PlanDecisionRecorded(payload)
            | Self::PlanPermissionGranted(payload)
            | Self::TaskCreatedFromPlan(payload)
            | Self::MutationPrepared(payload)
            | Self::MutationCommitted(payload)
            | Self::MutationReconciled(payload)
            | Self::MutationBatchStarted(payload)
            | Self::MutationBatchFinished(payload)
            | Self::WriteCommitted(payload)
            | Self::WorkspaceMutationDetected(payload)
            | Self::CheckpointRestored(payload)
            | Self::MutationArtifactCleanupRequested(payload)
            | Self::MutationArtifactLifecycleRecorded(payload)
            | Self::CommandFinished(payload)
            | Self::CheckFinished(payload)
            | Self::CheckSpecRecorded(payload)
            | Self::DiagnosticRecorded(payload)
            | Self::TodoChanged(payload)
            | Self::VerificationRecorded(payload)
            | Self::VerificationPolicyChanged(payload)
            | Self::VerificationCheckRun(payload)
            | Self::EnvironmentFingerprintRecorded(payload)
            | Self::ReadinessEvaluated(payload)
            | Self::TaskStatusChanged(payload)
            | Self::ChildVerificationReceiptLinked(payload)
            | Self::ChildChangesetMerged(payload)
            | Self::AgentMergeApplied(payload)
            | Self::WriteLeaseAcquired(payload)
            | Self::WriteLeaseReleased(payload)
            | Self::IsolatedWorkspaceCreated(payload)
            | Self::IsolatedChangeSetProduced(payload)
            | Self::MergeReviewRequested(payload)
            | Self::MergeReviewResolved(payload)
            | Self::JobIntentRecorded(payload)
            | Self::StepLeaseRecorded(payload)
            | Self::StepLeaseHeartbeatRecorded(payload)
            | Self::WorkspaceTrustDecision(payload)
            | Self::ContextSourceCaptured(payload)
            | Self::EgressDecisionRecorded(payload)
            | Self::ExtensionTrustDecision(payload)
            | Self::PluginHookExecutionStarted(payload)
            | Self::PluginHookExecutionFinished(payload)
            | Self::SandboxDecisionRecorded(payload)
            | Self::LogTailRecovered(payload) => Some(payload),
            Self::Legacy(_) => None,
        }
    }
}

pub type DomainEvent = DurableDomainEvent;

/// Stable view of one legacy `SessionLogEntry` line.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyEvent {
    pub event_id: EventId,
    pub session_id: SessionId,
    pub stream_sequence: u64,
    pub raw_line_hash: String,
    pub payload: Value,
}

#[derive(Debug)]
pub enum StoredEventDecode {
    Known(DomainEvent),
    UnknownNonCritical(StoredEvent),
}

#[derive(Debug)]
pub enum TypedStoredEventDecode {
    Known(Box<TypedDomainEvent>),
    UnknownNonCritical(Box<StoredEvent>),
}

#[derive(Debug, Clone)]
pub enum TypedDomainEvent {
    MutationPrepared(MutationPrepared),
    MutationCommitted(MutationCommitted),
    WorkspaceMutationDetected(WorkspaceMutationDetected),
    VerificationRecorded(VerificationRecordedEntry),
    VerificationCheckRun(VerificationCheckRunEntry),
    JobIntentRecorded(JobIntentEntry),
    StepLeaseRecorded(StepLeaseEntry),
    StepLeaseHeartbeatRecorded(StepLeaseHeartbeatEntry),
    TaskStatusChanged(ControlEntry),
    AgentThread(ControlEntry),
    TerminalTask(TerminalTaskEntry),
    ChangeSetProposed(ChangeSet),
    ChangeSetApplied(ChangeSetResult),
    WriteIsolation(ControlEntry),
    Other(DomainEvent),
}

pub fn decode_stored_event(event: StoredEvent) -> Result<StoredEventDecode> {
    event.verify_record_checksum()?;
    let Some(event_type) = event.event_kind() else {
        return match event.event_class {
            EventClass::NonCritical => Ok(StoredEventDecode::UnknownNonCritical(event)),
            EventClass::Critical => bail!("unknown critical event {}", event.event_type),
        };
    };
    if event_type == DurableEventType::Legacy {
        bail!("legacy is an upcast-only event type and cannot be decoded from StoredEvent");
    }
    let payload = DomainPayload {
        event_version: event.event_version,
        payload: event.payload,
    };
    Ok(StoredEventDecode::Known(domain_event_from_payload(
        event_type, payload,
    )))
}

pub fn decode_typed_stored_event(event: StoredEvent) -> Result<TypedStoredEventDecode> {
    event.verify_record_checksum()?;
    let Some(event_type) = event.event_kind() else {
        return match event.event_class {
            EventClass::NonCritical => {
                Ok(TypedStoredEventDecode::UnknownNonCritical(Box::new(event)))
            }
            EventClass::Critical => bail!("unknown critical event {}", event.event_type),
        };
    };
    if event_type == DurableEventType::Legacy {
        bail!("legacy is an upcast-only event type and cannot be decoded from StoredEvent");
    }
    let typed = match event_type {
        DurableEventType::MutationPrepared => {
            TypedDomainEvent::MutationPrepared(decode_event_payload(&event)?)
        }
        DurableEventType::MutationCommitted => {
            TypedDomainEvent::MutationCommitted(decode_event_payload(&event)?)
        }
        DurableEventType::WorkspaceMutationDetected => {
            TypedDomainEvent::WorkspaceMutationDetected(decode_event_payload(&event)?)
        }
        DurableEventType::VerificationRecorded => {
            TypedDomainEvent::VerificationRecorded(decode_verification_recorded(&event)?)
        }
        DurableEventType::VerificationCheckRun => {
            TypedDomainEvent::VerificationCheckRun(decode_verification_check_run(&event)?)
        }
        DurableEventType::JobIntentRecorded => {
            TypedDomainEvent::JobIntentRecorded(decode_job_intent_recorded(&event)?)
        }
        DurableEventType::StepLeaseRecorded => {
            TypedDomainEvent::StepLeaseRecorded(decode_step_lease_recorded(&event)?)
        }
        DurableEventType::StepLeaseHeartbeatRecorded => {
            TypedDomainEvent::StepLeaseHeartbeatRecorded(decode_step_lease_heartbeat_recorded(
                &event,
            )?)
        }
        DurableEventType::WriteLeaseAcquired
        | DurableEventType::WriteLeaseReleased
        | DurableEventType::IsolatedWorkspaceCreated
        | DurableEventType::IsolatedChangeSetProduced
        | DurableEventType::MergeReviewRequested
        | DurableEventType::MergeReviewResolved => {
            TypedDomainEvent::WriteIsolation(decode_write_isolation_record(&event)?)
        }
        DurableEventType::TaskStatusChanged => {
            let control = decode_control_entry(&event)?;
            match control {
                ControlEntry::TaskRun(_)
                | ControlEntry::TaskPlan(_)
                | ControlEntry::TaskStep(_) => TypedDomainEvent::TaskStatusChanged(control),
                _ => bail!("task status event carried non-task control payload"),
            }
        }
        DurableEventType::SessionEntryRecorded => {
            if let Some(control) = maybe_decode_control_entry(&event)? {
                match control {
                    ControlEntry::AgentThreadStarted(_)
                    | ControlEntry::AgentThreadStatusChanged(_)
                    | ControlEntry::AgentThreadMessageRouted(_)
                    | ControlEntry::AgentMailboxMessage(_)
                    | ControlEntry::AgentThreadResultRecorded(_)
                    | ControlEntry::AgentThreadResultDelivered(_)
                    | ControlEntry::AgentThreadDisplayName(_)
                    | ControlEntry::AgentThreadClosed(_) => TypedDomainEvent::AgentThread(control),
                    ControlEntry::TerminalTask(entry) => TypedDomainEvent::TerminalTask(entry),
                    ControlEntry::ChangeSetProposed(change_set) => {
                        TypedDomainEvent::ChangeSetProposed(change_set)
                    }
                    ControlEntry::ChangeSetApplied(result) => {
                        TypedDomainEvent::ChangeSetApplied(result)
                    }
                    ControlEntry::WriteLeaseAcquired(_)
                    | ControlEntry::WriteLeaseReleased(_)
                    | ControlEntry::IsolatedWorkspaceCreated(_)
                    | ControlEntry::IsolatedChangeSetProduced(_)
                    | ControlEntry::MergeReviewRequested(_)
                    | ControlEntry::MergeReviewResolved(_) => {
                        TypedDomainEvent::WriteIsolation(control)
                    }
                    _ => typed_other_event(event_type, event)?,
                }
            } else {
                typed_other_event(event_type, event)?
            }
        }
        _ => typed_other_event(event_type, event)?,
    };
    Ok(TypedStoredEventDecode::Known(Box::new(typed)))
}

fn typed_other_event(event_type: DurableEventType, event: StoredEvent) -> Result<TypedDomainEvent> {
    let payload = DomainPayload {
        event_version: event.event_version,
        payload: event.payload,
    };
    Ok(TypedDomainEvent::Other(domain_event_from_payload(
        event_type, payload,
    )))
}

fn decode_event_payload<T>(event: &StoredEvent) -> Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_value(event.payload.clone())
        .with_context(|| format!("failed to decode {} typed payload", event.event_type))
}

fn decode_verification_recorded(event: &StoredEvent) -> Result<VerificationRecordedEntry> {
    match decode_control_entry(event)? {
        ControlEntry::VerificationRecorded(entry) => Ok(entry),
        _ => bail!("verification recorded event carried non-verification payload"),
    }
}

fn decode_verification_check_run(event: &StoredEvent) -> Result<VerificationCheckRunEntry> {
    match decode_control_entry(event)? {
        ControlEntry::VerificationCheckRun(entry) => Ok(entry),
        _ => bail!("verification check run event carried non-check-run payload"),
    }
}

fn decode_job_intent_recorded(event: &StoredEvent) -> Result<JobIntentEntry> {
    match decode_control_entry(event)? {
        ControlEntry::JobIntentRecorded(entry) => Ok(entry),
        _ => bail!("job intent recorded event carried non-job-intent payload"),
    }
}

fn decode_step_lease_recorded(event: &StoredEvent) -> Result<StepLeaseEntry> {
    match decode_control_entry(event)? {
        ControlEntry::StepLeaseRecorded(entry) => Ok(entry),
        _ => bail!("step lease recorded event carried non-step-lease payload"),
    }
}

fn decode_step_lease_heartbeat_recorded(event: &StoredEvent) -> Result<StepLeaseHeartbeatEntry> {
    match decode_control_entry(event)? {
        ControlEntry::StepLeaseHeartbeatRecorded(entry) => Ok(entry),
        _ => bail!("step lease heartbeat event carried non-step-lease-heartbeat payload"),
    }
}

fn decode_write_isolation_record(event: &StoredEvent) -> Result<ControlEntry> {
    let control = decode_control_entry(event)?;
    let valid = matches!(
        (&event.event_kind(), &control),
        (
            Some(DurableEventType::WriteLeaseAcquired),
            ControlEntry::WriteLeaseAcquired(_)
        ) | (
            Some(DurableEventType::WriteLeaseReleased),
            ControlEntry::WriteLeaseReleased(_)
        ) | (
            Some(DurableEventType::IsolatedWorkspaceCreated),
            ControlEntry::IsolatedWorkspaceCreated(_)
        ) | (
            Some(DurableEventType::IsolatedChangeSetProduced),
            ControlEntry::IsolatedChangeSetProduced(_)
        ) | (
            Some(DurableEventType::MergeReviewRequested),
            ControlEntry::MergeReviewRequested(_)
        ) | (
            Some(DurableEventType::MergeReviewResolved),
            ControlEntry::MergeReviewResolved(_)
        )
    );
    if valid {
        Ok(control)
    } else {
        bail!(
            "{} event carried non-write-isolation control payload",
            event.event_type
        )
    }
}

fn decode_control_entry(event: &StoredEvent) -> Result<ControlEntry> {
    maybe_decode_control_entry(event)?.ok_or_else(|| {
        anyhow::anyhow!(
            "{} event did not contain a control session entry",
            event.event_type
        )
    })
}

fn maybe_decode_control_entry(event: &StoredEvent) -> Result<Option<ControlEntry>> {
    let Some(value) = event.payload.get("session_log_entry") else {
        return Ok(None);
    };
    let entry: SessionLogEntry = serde_json::from_value(value.clone())
        .with_context(|| format!("failed to decode {} session entry", event.event_type))?;
    match entry {
        SessionLogEntry::Control(control) => Ok(Some(control)),
        SessionLogEntry::User(_)
        | SessionLogEntry::Assistant(_)
        | SessionLogEntry::ToolResult(_) => Ok(None),
    }
}

fn domain_event_from_payload(
    event_type: DurableEventType,
    payload: DomainPayload,
) -> DurableDomainEvent {
    match event_type {
        DurableEventType::UserMessageRecorded => DurableDomainEvent::UserMessageRecorded(payload),
        DurableEventType::AssistantMessageRecorded => {
            DurableDomainEvent::AssistantMessageRecorded(payload)
        }
        DurableEventType::ToolResultRecorded => DurableDomainEvent::ToolResultRecorded(payload),
        DurableEventType::SessionEntryRecorded => DurableDomainEvent::SessionEntryRecorded(payload),
        DurableEventType::RunStatusChanged => DurableDomainEvent::RunStatusChanged(payload),
        DurableEventType::RunFinalized => DurableDomainEvent::RunFinalized(payload),
        DurableEventType::ToolExecutionStarted => DurableDomainEvent::ToolExecutionStarted(payload),
        DurableEventType::ToolExecutionFinished => {
            DurableDomainEvent::ToolExecutionFinished(payload)
        }
        DurableEventType::ApprovalResolved => DurableDomainEvent::ApprovalResolved(payload),
        DurableEventType::PlanDraftCreated => DurableDomainEvent::PlanDraftCreated(payload),
        DurableEventType::PlanDecisionRecorded => DurableDomainEvent::PlanDecisionRecorded(payload),
        DurableEventType::PlanPermissionGranted => {
            DurableDomainEvent::PlanPermissionGranted(payload)
        }
        DurableEventType::TaskCreatedFromPlan => DurableDomainEvent::TaskCreatedFromPlan(payload),
        DurableEventType::MutationPrepared => DurableDomainEvent::MutationPrepared(payload),
        DurableEventType::MutationCommitted => DurableDomainEvent::MutationCommitted(payload),
        DurableEventType::MutationReconciled => DurableDomainEvent::MutationReconciled(payload),
        DurableEventType::MutationBatchStarted => DurableDomainEvent::MutationBatchStarted(payload),
        DurableEventType::MutationBatchFinished => {
            DurableDomainEvent::MutationBatchFinished(payload)
        }
        DurableEventType::WriteCommitted => DurableDomainEvent::WriteCommitted(payload),
        DurableEventType::WorkspaceMutationDetected => {
            DurableDomainEvent::WorkspaceMutationDetected(payload)
        }
        DurableEventType::CheckpointRestored => DurableDomainEvent::CheckpointRestored(payload),
        DurableEventType::MutationArtifactCleanupRequested => {
            DurableDomainEvent::MutationArtifactCleanupRequested(payload)
        }
        DurableEventType::MutationArtifactLifecycleRecorded => {
            DurableDomainEvent::MutationArtifactLifecycleRecorded(payload)
        }
        DurableEventType::CommandFinished => DurableDomainEvent::CommandFinished(payload),
        DurableEventType::CheckFinished => DurableDomainEvent::CheckFinished(payload),
        DurableEventType::CheckSpecRecorded => DurableDomainEvent::CheckSpecRecorded(payload),
        DurableEventType::DiagnosticRecorded => DurableDomainEvent::DiagnosticRecorded(payload),
        DurableEventType::TodoChanged => DurableDomainEvent::TodoChanged(payload),
        DurableEventType::VerificationRecorded => DurableDomainEvent::VerificationRecorded(payload),
        DurableEventType::VerificationPolicyChanged => {
            DurableDomainEvent::VerificationPolicyChanged(payload)
        }
        DurableEventType::VerificationCheckRun => DurableDomainEvent::VerificationCheckRun(payload),
        DurableEventType::EnvironmentFingerprintRecorded => {
            DurableDomainEvent::EnvironmentFingerprintRecorded(payload)
        }
        DurableEventType::ReadinessEvaluated => DurableDomainEvent::ReadinessEvaluated(payload),
        DurableEventType::TaskStatusChanged => DurableDomainEvent::TaskStatusChanged(payload),
        DurableEventType::ChildVerificationReceiptLinked => {
            DurableDomainEvent::ChildVerificationReceiptLinked(payload)
        }
        DurableEventType::ChildChangesetMerged => DurableDomainEvent::ChildChangesetMerged(payload),
        DurableEventType::AgentMergeApplied => DurableDomainEvent::AgentMergeApplied(payload),
        DurableEventType::WriteLeaseAcquired => DurableDomainEvent::WriteLeaseAcquired(payload),
        DurableEventType::WriteLeaseReleased => DurableDomainEvent::WriteLeaseReleased(payload),
        DurableEventType::IsolatedWorkspaceCreated => {
            DurableDomainEvent::IsolatedWorkspaceCreated(payload)
        }
        DurableEventType::IsolatedChangeSetProduced => {
            DurableDomainEvent::IsolatedChangeSetProduced(payload)
        }
        DurableEventType::MergeReviewRequested => DurableDomainEvent::MergeReviewRequested(payload),
        DurableEventType::MergeReviewResolved => DurableDomainEvent::MergeReviewResolved(payload),
        DurableEventType::JobIntentRecorded => DurableDomainEvent::JobIntentRecorded(payload),
        DurableEventType::StepLeaseRecorded => DurableDomainEvent::StepLeaseRecorded(payload),
        DurableEventType::StepLeaseHeartbeatRecorded => {
            DurableDomainEvent::StepLeaseHeartbeatRecorded(payload)
        }
        DurableEventType::WorkspaceTrustDecision => {
            DurableDomainEvent::WorkspaceTrustDecision(payload)
        }
        DurableEventType::ContextSourceCaptured => {
            DurableDomainEvent::ContextSourceCaptured(payload)
        }
        DurableEventType::EgressDecisionRecorded => {
            DurableDomainEvent::EgressDecisionRecorded(payload)
        }
        DurableEventType::ExtensionTrustDecision => {
            DurableDomainEvent::ExtensionTrustDecision(payload)
        }
        DurableEventType::PluginHookExecutionStarted => {
            DurableDomainEvent::PluginHookExecutionStarted(payload)
        }
        DurableEventType::PluginHookExecutionFinished => {
            DurableDomainEvent::PluginHookExecutionFinished(payload)
        }
        DurableEventType::SandboxDecisionRecorded => {
            DurableDomainEvent::SandboxDecisionRecorded(payload)
        }
        DurableEventType::LogTailRecovered => DurableDomainEvent::LogTailRecovered(payload),
        DurableEventType::Legacy => unreachable!("legacy is handled before payload conversion"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReducerDisposition {
    Consumed(&'static str),
    ExplicitlyIgnored {
        reducer: &'static str,
        reason: &'static str,
    },
}

pub fn reducer_disposition(event_type: DurableEventType) -> ReducerDisposition {
    match event_type {
        DurableEventType::Legacy => ReducerDisposition::ExplicitlyIgnored {
            reducer: "legacy_upcast",
            reason: "legacy records are converted before domain reducers consume v2 events",
        },
        DurableEventType::ContextSourceCaptured => ReducerDisposition::ExplicitlyIgnored {
            reducer: "context_projection",
            reason: "context source events are indexed by future context projections",
        },
        _ => ReducerDisposition::Consumed("domain_event_projection"),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ProjectionCursor {
    pub session_id: SessionId,
    pub projection_schema_version: u16,
    pub last_applied_stream_sequence: u64,
    pub last_applied_event_id: EventId,
    pub last_applied_record_checksum: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionApplyDecision {
    Apply,
    IgnoreAlreadyApplied,
}

pub fn projection_apply_decision(
    cursor: Option<&ProjectionCursor>,
    event: &StoredEvent,
) -> Result<ProjectionApplyDecision> {
    projection_apply_decision_for_record(
        cursor,
        &event.session_id,
        event.stream_sequence,
        &event.event_id,
        &event.record_checksum,
    )
}

pub fn projection_apply_decision_for_record(
    cursor: Option<&ProjectionCursor>,
    session_id: &str,
    stream_sequence: u64,
    event_id: &str,
    record_checksum: &str,
) -> Result<ProjectionApplyDecision> {
    let Some(cursor) = cursor else {
        return Ok(ProjectionApplyDecision::Apply);
    };
    if cursor.session_id != session_id {
        bail!("projection cursor session does not match event session");
    }
    match stream_sequence.cmp(&cursor.last_applied_stream_sequence) {
        std::cmp::Ordering::Greater
            if stream_sequence == cursor.last_applied_stream_sequence + 1 =>
        {
            Ok(ProjectionApplyDecision::Apply)
        }
        std::cmp::Ordering::Greater => bail!("projection sequence gap"),
        std::cmp::Ordering::Equal
            if event_id == cursor.last_applied_event_id
                && record_checksum == cursor.last_applied_record_checksum =>
        {
            Ok(ProjectionApplyDecision::IgnoreAlreadyApplied)
        }
        std::cmp::Ordering::Less
            if event_id == cursor.last_applied_event_id
                && record_checksum == cursor.last_applied_record_checksum =>
        {
            Ok(ProjectionApplyDecision::IgnoreAlreadyApplied)
        }
        std::cmp::Ordering::Less => {
            bail!("projection cursor is ahead of event and cannot prove it was applied")
        }
        std::cmp::Ordering::Equal => {
            bail!("projection sequence conflict with different event id or checksum")
        }
    }
}

pub fn is_transient_run_event(event: &RunEvent) -> bool {
    matches!(
        event,
        RunEvent::TextDelta(_)
            | RunEvent::ReasoningDelta(_)
            | RunEvent::ToolCallArgsDelta { .. }
            | RunEvent::ToolProgress(_)
    )
}

pub fn is_v2_stored_event_value(value: &Value) -> bool {
    value.get("schema_version").is_some()
        && value.get("event_type").is_some()
        && value.get("stream_sequence").is_some()
        && value.get("record_checksum").is_some()
}

pub fn stable_event_hash(value: impl AsRef<[u8]>) -> String {
    let digest = Sha256::digest(value.as_ref());
    format!("sha256:{digest:x}")
}

pub fn stable_event_uuid(namespace: &str, value: &str) -> String {
    let namespace_uuid = Uuid::new_v5(&Uuid::NAMESPACE_OID, namespace.as_bytes());
    Uuid::new_v5(&namespace_uuid, value.as_bytes()).to_string()
}

fn ensure_payload_depth(value: &Value) -> Result<()> {
    let depth = payload_depth(value);
    if depth > MAX_PAYLOAD_DEPTH {
        bail!("stored event payload exceeds maximum nesting depth");
    }
    Ok(())
}

fn payload_depth(value: &Value) -> usize {
    match value {
        Value::Array(values) => 1 + values.iter().map(payload_depth).max().unwrap_or(0),
        Value::Object(object) => 1 + object.values().map(payload_depth).max().unwrap_or(0),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 1,
    }
}

fn canonical_json_bytes(value: &Value) -> Result<Vec<u8>> {
    let canonical = canonicalize_value(value)?;
    serde_json::to_vec(&canonical).context("failed to serialize canonical json")
}

fn canonicalize_value(value: &Value) -> Result<Value> {
    match value {
        Value::Array(values) => values
            .iter()
            .map(canonicalize_value)
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(object) => {
            let ordered = object
                .iter()
                .map(|(key, value)| canonicalize_value(value).map(|value| (key.clone(), value)))
                .collect::<Result<BTreeMap<_, _>>>()?;
            Ok(Value::Object(ordered.into_iter().collect()))
        }
        Value::Number(number) => Ok(Value::Number(canonicalize_number(number)?)),
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(value.clone()),
    }
}

fn canonicalize_number(number: &serde_json::Number) -> Result<serde_json::Number> {
    if number.as_i64().is_some() || number.as_u64().is_some() {
        return Ok(number.clone());
    }
    let value = number
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("stored event number is not representable as f64"))?;
    if !value.is_finite() {
        bail!("stored event number is not finite");
    }
    if value.fract() == 0.0 && value >= i64::MIN as f64 && value <= i64::MAX as f64 {
        return Ok(serde_json::Number::from(value as i64));
    }
    Ok(number.clone())
}

/// Structured runtime events emitted by the agent loop for UI, logging, and orchestration.
#[derive(Debug, Clone)]
pub enum RunEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallStarted(ToolCall),
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallCompleted(ToolCall),
    ToolApprovalRequested {
        call: ToolCall,
        spec: ToolSpec,
        subjects: Vec<ToolSubject>,
        operation: ToolOperation,
        risk: PermissionRisk,
        subject_zones: Vec<PathTrustZone>,
        confirmation: Option<PermissionConfirmation>,
        snapshot_required: bool,
        preview: Option<ToolPreview>,
    },
    ToolApprovalResolved {
        call_id: String,
        approved: bool,
        reason: Option<String>,
    },
    ToolProgress(ToolProgressEvent),
    ToolResult(ToolResult),
    Usage(UsageStats),
    ContinuationState(ProviderContinuationState),
    Control(ControlEntry),
    AssistantMessage(ModelMessage),
    Notice(String),
}

/// Stable, versioned event envelope for TUI, CLI, HTTP, and future adapter surfaces.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicRunEvent {
    pub schema_version: u32,
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event: PublicRunEventKind,
}

impl PublicRunEvent {
    /// Creates a public run event with the current schema version.
    pub fn new(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        sequence: u64,
        event: PublicRunEventKind,
    ) -> Self {
        Self {
            schema_version: PUBLIC_RUN_EVENT_SCHEMA_VERSION,
            session_id: session_id.into(),
            run_id: run_id.into(),
            sequence,
            event,
        }
    }

    /// Projects one internal run event into the stable public envelope.
    pub fn from_run_event(
        session_id: impl Into<String>,
        run_id: impl Into<String>,
        sequence: u64,
        event: RunEvent,
    ) -> Self {
        Self::new(session_id, run_id, sequence, event.into())
    }
}

/// Public event payloads exposed to external run consumers.
///
/// Lifecycle events are owned by adapters because the kernel's internal [`RunEvent`] stream only
/// represents events produced inside an already-running agent loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PublicRunEventKind {
    RunStarted {
        prompt: String,
    },
    TaskRunStarted {
        task_id: String,
        objective: String,
    },
    RunFinished {
        final_text: String,
    },
    TaskRunFinished {
        task_id: String,
        status: String,
    },
    RunFailed {
        error: String,
    },
    RunCancelled,
    TextDelta {
        text: String,
    },
    ReasoningDelta {
        text: String,
    },
    ToolCallStarted {
        call: ToolCall,
    },
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallCompleted {
        call: ToolCall,
    },
    ApprovalRequested {
        call: ToolCall,
        spec: ToolSpec,
        subjects: Vec<ToolSubject>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        operation: Option<ToolOperation>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        risk: Option<PermissionRisk>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        subject_zones: Vec<PathTrustZone>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confirmation: Option<PermissionConfirmation>,
        #[serde(default)]
        snapshot_required: bool,
        preview: Option<ToolPreview>,
    },
    ApprovalResolved {
        call_id: String,
        approved: bool,
        reason: Option<String>,
    },
    ToolResult {
        result: ToolResult,
    },
    ToolProgress {
        progress: ToolProgressEvent,
    },
    Usage {
        usage: UsageStats,
    },
    ContinuationState {
        state: ProviderContinuationState,
    },
    Control {
        control: PublicControlEvent,
    },
    AssistantMessage {
        message: PublicAssistantMessage,
    },
    Notice {
        message: String,
    },
}

/// Public projection of a control-plane event.
///
/// The `kind` field is the stable routing surface. `payload` is an opaque JSON projection for
/// adapters that need diagnostic detail before a dedicated public event variant exists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicControlEvent {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl From<ControlEntry> for PublicControlEvent {
    fn from(entry: ControlEntry) -> Self {
        let kind = control_entry_kind(&entry).to_owned();
        let payload = serde_json::to_value(&entry).ok();
        Self { kind, payload }
    }
}

/// Public projection of a completed assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublicAssistantMessage {
    pub id: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_kind: Option<crate::AssistantMessageKind>,
}

impl From<ModelMessage> for PublicAssistantMessage {
    fn from(message: ModelMessage) -> Self {
        Self {
            id: message.id,
            content: message.content,
            tool_calls: message.tool_calls,
            assistant_kind: message.assistant_kind,
        }
    }
}

impl From<RunEvent> for PublicRunEventKind {
    fn from(event: RunEvent) -> Self {
        match event {
            RunEvent::TextDelta(text) => Self::TextDelta { text },
            RunEvent::ReasoningDelta(text) => Self::ReasoningDelta { text },
            RunEvent::ToolCallStarted(call) => Self::ToolCallStarted { call },
            RunEvent::ToolCallArgsDelta { id, delta } => Self::ToolCallArgsDelta { id, delta },
            RunEvent::ToolCallCompleted(call) => Self::ToolCallCompleted { call },
            RunEvent::ToolApprovalRequested {
                call,
                spec,
                subjects,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                preview,
            } => Self::ApprovalRequested {
                call,
                spec,
                subjects,
                operation: Some(operation),
                risk: Some(risk),
                subject_zones,
                confirmation,
                snapshot_required,
                preview,
            },
            RunEvent::ToolApprovalResolved {
                call_id,
                approved,
                reason,
            } => Self::ApprovalResolved {
                call_id,
                approved,
                reason,
            },
            RunEvent::ToolProgress(progress) => Self::ToolProgress { progress },
            RunEvent::ToolResult(result) => Self::ToolResult { result },
            RunEvent::Usage(usage) => Self::Usage { usage },
            RunEvent::ContinuationState(state) => Self::ContinuationState { state },
            RunEvent::Control(entry) => Self::Control {
                control: entry.into(),
            },
            RunEvent::AssistantMessage(message) => Self::AssistantMessage {
                message: message.into(),
            },
            RunEvent::Notice(message) => Self::Notice { message },
        }
    }
}

fn control_entry_kind(entry: &ControlEntry) -> &'static str {
    match entry {
        ControlEntry::SessionIdentity { .. } => "session_identity",
        ControlEntry::ContinuationStateSaved(_) => "continuation_state_saved",
        ControlEntry::ResponseHandleTracked(_) => "response_handle_tracked",
        ControlEntry::BackgroundTaskTracked(_) => "background_task_tracked",
        ControlEntry::PrefixSnapshotCaptured(_) => "prefix_snapshot_captured",
        ControlEntry::MemorySnapshotCaptured(_) => "memory_snapshot_captured",
        ControlEntry::ContextAssemblySkipped(_) => "context_assembly_skipped",
        ControlEntry::UsageSnapshot(_) => "usage_snapshot",
        ControlEntry::ToolApproval(_) => "tool_approval",
        ControlEntry::ToolApprovalSessionGrant(_) => "tool_approval_session_grant",
        ControlEntry::ToolExecution(_) => "tool_execution",
        ControlEntry::ToolEgress(_) => "tool_egress",
        ControlEntry::McpElicitation(_) => "mcp_elicitation",
        ControlEntry::ToolPreviewCaptured(_) => "tool_preview_captured",
        ControlEntry::SkillIndexCaptured(_) => "skill_index_captured",
        ControlEntry::SkillLoaded(_) => "skill_loaded",
        ControlEntry::PluginManifestCaptured(_) => "plugin_manifest_captured",
        ControlEntry::PluginTrustDecision(_) => "plugin_trust_decision",
        ControlEntry::PluginHookExecutionStarted(_) => "plugin_hook_execution_started",
        ControlEntry::PluginHookExecutionFinished(_) => "plugin_hook_execution_finished",
        ControlEntry::ChangeSetProposed(_) => "change_set_proposed",
        ControlEntry::ChangeSetApplied(_) => "change_set_applied",
        ControlEntry::TerminalTask(_) => "terminal_task",
        ControlEntry::CompactionApplied(_) => "compaction_applied",
        ControlEntry::PlanApproved(_) => "plan_approved",
        ControlEntry::PlanDraftCreated(_) => "plan_draft_created",
        ControlEntry::PlanDecisionRecorded(_) => "plan_decision_recorded",
        ControlEntry::PlanPermissionGranted(_) => "plan_permission_granted",
        ControlEntry::TaskCreatedFromPlan(_) => "task_created_from_plan",
        ControlEntry::TaskRun(_) => "task_run",
        ControlEntry::TaskPlan(_) => "task_plan",
        ControlEntry::TaskStep(_) => "task_step",
        ControlEntry::TaskChildSession(_) => "task_child_session",
        ControlEntry::TaskChildSessionDisplayName(_) => "task_child_session_display_name",
        ControlEntry::TaskSubagentApprovalRoute(_) => "task_subagent_approval_route",
        ControlEntry::TaskSubagentElicitationRoute(_) => "task_subagent_elicitation_route",
        ControlEntry::JobIntentRecorded(_) => "job_intent_recorded",
        ControlEntry::StepLeaseRecorded(_) => "step_lease_recorded",
        ControlEntry::StepLeaseHeartbeatRecorded(_) => "step_lease_heartbeat_recorded",
        ControlEntry::CheckSpecRecorded(_) => "check_spec_recorded",
        ControlEntry::VerificationPolicyChanged(_) => "verification_policy_changed",
        ControlEntry::VerificationCheckRun(_) => "verification_check_run",
        ControlEntry::VerificationRecorded(_) => "verification_recorded",
        ControlEntry::ReadinessEvaluated(_) => "readiness_evaluated",
        ControlEntry::ChildVerificationReceiptLinked(_) => "child_verification_receipt_linked",
        ControlEntry::WorkspaceTrustDecision(_) => "workspace_trust_decision",
        ControlEntry::WriteLeaseAcquired(_) => "write_lease_acquired",
        ControlEntry::WriteLeaseReleased(_) => "write_lease_released",
        ControlEntry::IsolatedWorkspaceCreated(_) => "isolated_workspace_created",
        ControlEntry::IsolatedChangeSetProduced(_) => "isolated_changeset_produced",
        ControlEntry::MergeReviewRequested(_) => "merge_review_requested",
        ControlEntry::MergeReviewResolved(_) => "merge_review_resolved",
        ControlEntry::AgentProfileCaptured(_) => "agent_profile_captured",
        ControlEntry::AgentProfileTrustDecision(_) => "agent_profile_trust_decision",
        ControlEntry::AgentProfilePolicyDecision(_) => "agent_profile_policy_decision",
        ControlEntry::AgentThreadStarted(_) => "agent_thread_started",
        ControlEntry::AgentThreadStatusChanged(_) => "agent_thread_status_changed",
        ControlEntry::AgentThreadMessageRouted(_) => "agent_thread_message_routed",
        ControlEntry::AgentMailboxMessage(_) => "agent_mailbox_message",
        ControlEntry::AgentThreadResultRecorded(_) => "agent_thread_result_recorded",
        ControlEntry::AgentThreadResultDelivered(_) => "agent_thread_result_delivered",
        ControlEntry::AgentResultContinuation(_) => "agent_result_continuation",
        ControlEntry::AgentThreadDisplayName(_) => "agent_thread_display_name",
        ControlEntry::AgentApprovalRoute(_) => "agent_approval_route",
        ControlEntry::AgentElicitationRoute(_) => "agent_elicitation_route",
        ControlEntry::AgentRunAttemptStarted(_) => "agent_run_attempt_started",
        ControlEntry::AgentRunHeartbeat(_) => "agent_run_heartbeat",
        ControlEntry::AgentRunInterrupted(_) => "agent_run_interrupted",
        ControlEntry::AgentRouteClosed(_) => "agent_route_closed",
        ControlEntry::AgentMergeSafePoint(_) => "agent_merge_safe_point",
        ControlEntry::AgentThreadClosed(_) => "agent_thread_closed",
        ControlEntry::ConversationInputQueued(_) => "conversation_input_queued",
        ControlEntry::ConversationInputQueueControl(_) => "conversation_input_queue_control",
        ControlEntry::ConversationInputEdited(_) => "conversation_input_edited",
        ControlEntry::ConversationInputReordered(_) => "conversation_input_reordered",
        ControlEntry::ConversationInputStatusChanged(_) => "conversation_input_status_changed",
        ControlEntry::Note { .. } => "note",
    }
}

/// Sink for run events emitted by the agent loop.
pub trait EventHandler {
    /// Handles one run event.
    ///
    /// # Errors
    ///
    /// Returns an error when the downstream event consumer fails and the current run should stop.
    fn handle(&mut self, event: RunEvent) -> Result<()>;
}

/// Event handler that ignores every incoming event.
pub struct NoopEventHandler;

impl EventHandler for NoopEventHandler {
    fn handle(&mut self, _event: RunEvent) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tests/event_tests.rs"]
mod tests;
