use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ApprovalMode, ChangeSet, ChangeSetResult, CommandPermissionMatch, ControlEntry,
    EgressDisclosurePresented, HostedToolAuthorization, HostedToolOutcome, JobIntentEntry,
    McpTransportAuthorization, ModelMessage, MutationCommitted, MutationPrepared, NetworkEffect,
    PathTrustZone, PermissionConfirmation, PermissionRisk, ProviderContinuationState,
    QueryEgressOutcome, QueryEgressStarted, SessionLogEntry, StepLeaseEntry,
    StepLeaseHeartbeatEntry, TerminalTaskEntry, ToolCall, ToolOperation, ToolPreview,
    ToolProgressEvent, ToolResult, ToolSpec, ToolSubject, UsageStats, VerificationCheckRunEntry,
    VerificationRecordedEntry, WebFetchTransportAuthorization, WorkspaceMutationDetected,
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

/// Versioned payload carried by a reducer-facing domain event.
#[derive(Debug, Clone, PartialEq)]
pub struct DomainPayload {
    pub event_version: u16,
    pub payload: Value,
}

/// Physical storage shape for one durable event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableEventPayloadStorage {
    /// Payload is a JSON object with a `session_log_entry` field.
    SessionLogEntry,
    /// Payload is a durable-event-specific JSON object.
    DirectJson,
}

/// Static payload metadata for a durable event type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurableEventPayloadMetadata {
    pub storage: DurableEventPayloadStorage,
    pub payload_name: &'static str,
}

/// Stable deserialize-only view of one legacy `SessionLogEntry` line.
#[derive(Debug, Clone, PartialEq)]
pub struct LegacyEvent {
    pub event_id: EventId,
    pub session_id: SessionId,
    pub stream_sequence: u64,
    pub raw_line_hash: String,
    pub payload: Value,
}

macro_rules! durable_event_types {
    ($($variant:ident => ($wire_name:literal, $sync_class:ident, $event_class:ident, $payload_storage:ident, $payload_name:literal),)+) => {
        /// Known durable event type names.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum DurableEventType {
            $($variant,)+
            Legacy,
        }

        /// Strong reducer-facing event.
        #[derive(Debug, Clone, PartialEq)]
        pub enum DurableDomainEvent {
            $($variant(DomainPayload),)+
            Legacy(LegacyEvent),
        }

        impl DurableEventType {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire_name,)+
                    Self::Legacy => "legacy",
                }
            }

            pub fn from_event_type(value: &str) -> Option<Self> {
                Some(match value {
                    $($wire_name => Self::$variant,)+
                    "legacy" => Self::Legacy,
                    _ => return None,
                })
            }

            pub fn sync_class(self) -> Option<EventSyncClass> {
                match self {
                    $(Self::$variant => Some(EventSyncClass::$sync_class),)+
                    Self::Legacy => None,
                }
            }

            pub fn expected_event_class(self) -> Option<EventClass> {
                match self {
                    $(Self::$variant => Some(EventClass::$event_class),)+
                    Self::Legacy => None,
                }
            }

            pub fn payload_metadata(self) -> DurableEventPayloadMetadata {
                match self {
                    $(Self::$variant => DurableEventPayloadMetadata {
                        storage: DurableEventPayloadStorage::$payload_storage,
                        payload_name: $payload_name,
                    },)+
                    Self::Legacy => DurableEventPayloadMetadata {
                        storage: DurableEventPayloadStorage::SessionLogEntry,
                        payload_name: "legacy_session_entry",
                    },
                }
            }

            pub fn appendable(self) -> bool {
                self != Self::Legacy
            }

            pub fn to_domain_event(self, payload: DomainPayload) -> Result<DurableDomainEvent> {
                match self {
                    $(Self::$variant => Ok(DurableDomainEvent::$variant(payload)),)+
                    Self::Legacy => bail!("legacy is deserialize-only and cannot be a v2 event"),
                }
            }
        }

        impl DurableDomainEvent {
            pub fn event_type(&self) -> DurableEventType {
                match self {
                    $(Self::$variant(_) => DurableEventType::$variant,)+
                    Self::Legacy(_) => DurableEventType::Legacy,
                }
            }

            pub fn payload(&self) -> Option<&DomainPayload> {
                match self {
                    $(Self::$variant(payload) => Some(payload),)+
                    Self::Legacy(_) => None,
                }
            }
        }

        /// Ordered known durable event types.
        pub const ALL_DURABLE_EVENT_TYPES: &[DurableEventType] = &[
            $(DurableEventType::$variant,)+
            DurableEventType::Legacy,
        ];
    };
}

durable_event_types! {
    UserMessageRecorded => ("user_message_recorded", NormalEvent, Critical, SessionLogEntry, "session_log_entry"),
    AssistantMessageRecorded => ("assistant_message_recorded", NormalEvent, Critical, SessionLogEntry, "session_log_entry"),
    ToolResultRecorded => ("tool_result_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    SessionEntryRecorded => ("session_entry_recorded", RecoveryCritical, NonCritical, SessionLogEntry, "session_log_entry"),
    RunStatusChanged => ("run_status_changed", RecoveryCritical, Critical, DirectJson, "run_lifecycle"),
    RunFinalized => ("run_finalized", RecoveryCritical, Critical, DirectJson, "run_lifecycle"),
    ToolExecutionStarted => ("tool_execution_started", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ToolExecutionFinished => ("tool_execution_finished", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ApprovalResolved => ("approval_resolved", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    PlanDraftCreated => ("plan_draft_created", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    PlanDecisionRecorded => ("plan_decision_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    PlanPermissionGranted => ("plan_permission_granted", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    TaskCreatedFromPlan => ("task_created_from_plan", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    MutationPrepared => ("mutation_prepared", RecoveryCritical, Critical, DirectJson, "mutation_prepared"),
    MutationCommitted => ("mutation_committed", RecoveryCritical, Critical, DirectJson, "mutation_committed"),
    MutationReconciled => ("mutation_reconciled", RecoveryCritical, Critical, DirectJson, "mutation_reconciled"),
    MutationBatchStarted => ("mutation_batch_started", RecoveryCritical, Critical, DirectJson, "mutation_batch_started"),
    MutationBatchFinished => ("mutation_batch_finished", RecoveryCritical, Critical, DirectJson, "mutation_batch_finished"),
    WriteCommitted => ("write_committed", RecoveryCritical, Critical, DirectJson, "write_committed"),
    WorkspaceMutationDetected => ("workspace_mutation_detected", RecoveryCritical, Critical, DirectJson, "workspace_mutation_detected"),
    CheckpointRestored => ("checkpoint_restored", RecoveryCritical, Critical, DirectJson, "checkpoint_restored"),
    MutationArtifactCleanupRequested => ("mutation_artifact_cleanup_requested", RecoveryCritical, Critical, DirectJson, "mutation_artifact_cleanup_requested"),
    MutationArtifactLifecycleRecorded => ("mutation_artifact_lifecycle_recorded", RecoveryCritical, Critical, DirectJson, "mutation_artifact_lifecycle_recorded"),
    CommandFinished => ("command_finished", RecoveryCritical, Critical, DirectJson, "command_finished"),
    CheckFinished => ("check_finished", RecoveryCritical, Critical, DirectJson, "check_finished"),
    CheckSpecRecorded => ("check_spec_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    DiagnosticRecorded => ("diagnostic_recorded", RecoveryCritical, Critical, DirectJson, "diagnostic_recorded"),
    TodoChanged => ("todo_changed", RecoveryCritical, Critical, DirectJson, "todo_changed"),
    VerificationRecorded => ("verification_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    VerificationPolicyChanged => ("verification_policy_changed", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    VerificationCheckRun => ("verification_check_run", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    EnvironmentFingerprintRecorded => ("environment_fingerprint_recorded", RecoveryCritical, Critical, DirectJson, "environment_fingerprint_recorded"),
    ReadinessEvaluated => ("readiness_evaluated", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    TaskStatusChanged => ("task_status_changed", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ChildVerificationReceiptLinked => ("child_verification_receipt_linked", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ChildChangesetMerged => ("child_changeset_merged", RecoveryCritical, Critical, DirectJson, "child_changeset_merged"),
    AgentMergeApplied => ("agent_merge_applied", RecoveryCritical, Critical, DirectJson, "agent_merge_applied"),
    WriteLeaseAcquired => ("write_lease_acquired", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    WriteLeaseReleased => ("write_lease_released", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    IsolatedWorkspaceCreated => ("isolated_workspace_created", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    IsolatedChangeSetProduced => ("isolated_changeset_produced", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    MergeReviewRequested => ("merge_review_requested", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    MergeReviewResolved => ("merge_review_resolved", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    JobIntentRecorded => ("job_intent_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    StepLeaseRecorded => ("step_lease_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    StepLeaseHeartbeatRecorded => ("step_lease_heartbeat_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    WorkspaceTrustDecision => ("workspace_trust_decision", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ContextSourceCaptured => ("context_source_captured", NormalEvent, NonCritical, SessionLogEntry, "session_log_entry"),
    EgressDecisionRecorded => ("egress_decision_recorded", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ExtensionTrustDecision => ("extension_trust_decision", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    PluginHookExecutionStarted => ("plugin_hook_execution_started", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    PluginHookExecutionFinished => ("plugin_hook_execution_finished", RecoveryCritical, Critical, SessionLogEntry, "session_log_entry"),
    ExtensionProcessLifecycleRecorded => ("extension_process_lifecycle_recorded", RecoveryCritical, Critical, DirectJson, "extension_process_lifecycle"),
    HostedToolAuthorization => ("hosted_tool_authorization", RecoveryCritical, Critical, DirectJson, "hosted_tool_authorization"),
    HostedToolOutcome => ("hosted_tool_outcome", RecoveryCritical, Critical, DirectJson, "hosted_tool_outcome"),
    McpTransportAuthorization => ("mcp_transport_authorization", RecoveryCritical, Critical, DirectJson, "mcp_transport_authorization"),
    WebFetchTransportAuthorization => ("web_fetch_transport_authorization", RecoveryCritical, Critical, DirectJson, "web_fetch_transport_authorization"),
    EgressDisclosurePresented => ("egress_disclosure_presented", RecoveryCritical, Critical, DirectJson, "egress_disclosure_presented"),
    QueryEgressStarted => ("query_egress_started", RecoveryCritical, Critical, DirectJson, "query_egress_started"),
    QueryEgressOutcome => ("query_egress_outcome", RecoveryCritical, Critical, DirectJson, "query_egress_outcome"),
    SandboxDecisionRecorded => ("sandbox_decision_recorded", RecoveryCritical, Critical, DirectJson, "sandbox_decision_recorded"),
    LogTailRecovered => ("log_tail_recovered", TailRecovery, Critical, DirectJson, "log_tail_recovered"),
}

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

pub type DomainEvent = DurableDomainEvent;

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
    HostedToolAuthorization(HostedToolAuthorization),
    HostedToolOutcome(HostedToolOutcome),
    McpTransportAuthorization(McpTransportAuthorization),
    WebFetchTransportAuthorization(WebFetchTransportAuthorization),
    EgressDisclosurePresented(EgressDisclosurePresented),
    QueryEgressStarted(QueryEgressStarted),
    QueryEgressOutcome(QueryEgressOutcome),
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
    let payload = DomainPayload {
        event_version: event.event_version,
        payload: event.payload,
    };
    Ok(StoredEventDecode::Known(
        event_type.to_domain_event(payload)?,
    ))
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
        DurableEventType::HostedToolAuthorization => {
            let entry: HostedToolAuthorization = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::HostedToolAuthorization(entry)
        }
        DurableEventType::HostedToolOutcome => {
            let entry: HostedToolOutcome = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::HostedToolOutcome(entry)
        }
        DurableEventType::McpTransportAuthorization => {
            let entry: McpTransportAuthorization = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::McpTransportAuthorization(entry)
        }
        DurableEventType::WebFetchTransportAuthorization => {
            let entry: WebFetchTransportAuthorization = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::WebFetchTransportAuthorization(entry)
        }
        DurableEventType::EgressDisclosurePresented => {
            let entry: EgressDisclosurePresented = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::EgressDisclosurePresented(entry)
        }
        DurableEventType::QueryEgressStarted => {
            let entry: QueryEgressStarted = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::QueryEgressStarted(entry)
        }
        DurableEventType::QueryEgressOutcome => {
            let entry: QueryEgressOutcome = decode_event_payload(&event)?;
            entry.validate().map_err(anyhow::Error::from)?;
            TypedDomainEvent::QueryEgressOutcome(entry)
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
    Ok(TypedDomainEvent::Other(
        event_type.to_domain_event(payload)?,
    ))
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
        network_effect: Option<NetworkEffect>,
        local_policy_decision: ApprovalMode,
        network_policy_decision: ApprovalMode,
        source_policy_decision: ApprovalMode,
        operation: ToolOperation,
        risk: PermissionRisk,
        subject_zones: Vec<PathTrustZone>,
        confirmation: Option<PermissionConfirmation>,
        snapshot_required: bool,
        command_permission_matches: Vec<CommandPermissionMatch>,
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
        network_effect: Option<NetworkEffect>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_policy_decision: Option<ApprovalMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        network_policy_decision: Option<ApprovalMode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_policy_decision: Option<ApprovalMode>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command_permission_matches: Vec<CommandPermissionMatch>,
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
                network_effect,
                local_policy_decision,
                network_policy_decision,
                source_policy_decision,
                operation,
                risk,
                subject_zones,
                confirmation,
                snapshot_required,
                command_permission_matches,
                preview,
            } => Self::ApprovalRequested {
                call,
                spec,
                subjects,
                network_effect,
                local_policy_decision: Some(local_policy_decision),
                network_policy_decision: Some(network_policy_decision),
                source_policy_decision: Some(source_policy_decision),
                operation: Some(operation),
                risk: Some(risk),
                subject_zones,
                confirmation,
                snapshot_required,
                command_permission_matches,
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
        ControlEntry::ExternalProvenance(_) => "external_provenance",
        ControlEntry::WebUrlCapabilityDescriptor(_) => "web_url_capability_descriptor",
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
