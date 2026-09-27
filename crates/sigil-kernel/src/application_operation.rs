//! Causal application-operation receipts issued by the existing session domain owner.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::{ControlEntry, ConversationInputStatus, PlanDecision};

/// Exact structured transition that may settle one admitted command. No free-form command or
/// user text is interpreted by the host to select this target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ApplicationOperationTargetV1 {
    ForkConversation {
        source_turn_digest: String,
        connection_id: String,
        model_id: String,
    },
    ImportBranchKnowledge {
        source_session_id: String,
        source_turn_digest: String,
        source_message_id: String,
        source_text_sha256: String,
        summary_sha256: String,
    },
    QueueEnqueue {
        prompt_hash: String,
        target: crate::ConversationInputTarget,
        input_kind: crate::ConversationInputKind,
    },
    QueueEdit {
        queue_id: String,
        prompt_hash: String,
    },
    QueueCancel {
        queue_id: String,
    },
    QueueReorder {
        queue_id: String,
    },
    QueuePause {
        paused: bool,
    },
    PlanDecision {
        plan_id: String,
        plan_hash: String,
        decision: PlanDecision,
    },
    PlanAdoption {
        plan_id: String,
        plan_hash: String,
    },
    PlanCandidateAdoption {
        plan_id: String,
        candidate_hash: String,
    },
    PlanRevisionGuidance {
        plan_id: String,
        plan_hash: String,
    },
    UserInputContinuation {
        original_operation_id: String,
        request_id: String,
        generation: u32,
        request_hash: String,
    },
    UserInputDecision {
        request_id: String,
        generation: u32,
        request_hash: String,
        command_id: String,
    },
}

impl ApplicationOperationTargetV1 {
    pub(crate) fn matches(&self, control: &ControlEntry) -> bool {
        match (self, control) {
            (
                Self::ForkConversation {
                    source_turn_digest,
                    connection_id,
                    model_id,
                },
                ControlEntry::ConversationForkCommittedV1(entry),
            ) => {
                &entry.source_turn_digest == source_turn_digest
                    && entry.target_model_ref.connection_id.as_str() == connection_id
                    && &entry.target_model_ref.model_id == model_id
            }
            (
                Self::ImportBranchKnowledge {
                    source_session_id,
                    source_turn_digest,
                    source_message_id,
                    source_text_sha256,
                    summary_sha256,
                },
                ControlEntry::BranchKnowledgeImportedV1(entry),
            ) => {
                entry.source_session_id == *source_session_id
                    && entry.source_turn_digest == *source_turn_digest
                    && entry.source_message_id == *source_message_id
                    && entry.source_text_sha256 == *source_text_sha256
                    && entry.summary_sha256 == *summary_sha256
            }
            (
                Self::QueueEnqueue {
                    prompt_hash,
                    target,
                    input_kind,
                },
                ControlEntry::ConversationInputQueued(entry),
            ) => {
                &entry.prompt_hash == prompt_hash
                    && &entry.target == target
                    && &entry.kind == input_kind
            }
            (
                Self::QueueEdit {
                    queue_id,
                    prompt_hash,
                },
                ControlEntry::ConversationInputEdited(entry),
            ) => entry.queue_id.as_str() == queue_id && &entry.prompt_hash == prompt_hash,
            (
                Self::QueueCancel { queue_id },
                ControlEntry::ConversationInputStatusChanged(entry),
            ) => {
                entry.queue_id.as_str() == queue_id
                    && entry.status == ConversationInputStatus::Cancelled
            }
            (Self::QueueReorder { queue_id }, ControlEntry::ConversationInputReordered(entry)) => {
                entry.queue_id.as_str() == queue_id
            }
            (Self::QueuePause { paused }, ControlEntry::ConversationInputQueueControl(entry)) => {
                matches!(
                    (paused, entry.action),
                    (true, crate::ConversationInputQueueControlAction::Pause)
                        | (false, crate::ConversationInputQueueControlAction::Resume)
                )
            }
            (
                Self::PlanDecision {
                    plan_id,
                    plan_hash,
                    decision,
                },
                ControlEntry::PlanDecisionRecorded(entry),
            ) => {
                entry.plan_id.as_str() == plan_id
                    && &entry.plan_hash == plan_hash
                    && &entry.decision == decision
            }
            (
                Self::PlanAdoption { plan_id, plan_hash },
                ControlEntry::TaskCreatedFromPlan(entry),
            ) => entry.plan_id.as_str() == plan_id && &entry.plan_hash == plan_hash,
            (
                Self::PlanCandidateAdoption {
                    plan_id,
                    candidate_hash,
                },
                ControlEntry::PlanDraftCreated(entry),
            ) => entry.plan_id.as_str() == plan_id && &entry.plan_hash == candidate_hash,
            (
                Self::PlanRevisionGuidance { plan_id, plan_hash },
                ControlEntry::UserInputRequested(entry),
            ) => {
                matches!(&entry.request.source, crate::UserInputSourceV1::PlanRevision { base_plan_id, base_plan_hash } if base_plan_id.as_str() == plan_id && base_plan_hash == plan_hash)
            }
            (
                Self::UserInputContinuation {
                    request_id,
                    generation,
                    request_hash,
                    ..
                },
                ControlEntry::UserInputContinuationStarted(entry),
            ) => {
                entry.identity.request_id.as_str() == request_id
                    && entry.identity.generation == *generation
                    && &entry.request_hash == request_hash
            }
            (
                Self::UserInputDecision {
                    request_id,
                    generation,
                    request_hash,
                    command_id,
                },
                ControlEntry::UserInputDecisionAccepted(entry),
            ) => {
                entry.identity.request_id.as_str() == request_id
                    && &entry.identity.generation == generation
                    && &entry.request_hash == request_hash
                    && entry.command_id.as_str() == command_id
            }
            _ => false,
        }
    }
}

/// K remains an application contract; kernel stores only its canonical digest and F.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationOperationBindingV1 {
    pub schema_version: u16,
    /// Original application authority; never rewritten for a child domain.
    pub session_scope_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain_session_scope_id: Option<String>,
    pub reservation_key_digest: String,
    pub fingerprint: String,
    pub operation_id: String,
    pub target: ApplicationOperationTargetV1,
}

impl ApplicationOperationBindingV1 {
    pub fn new(
        session_scope_id: String,
        reservation_key_digest: String,
        fingerprint: String,
        target: ApplicationOperationTargetV1,
    ) -> Result<Self> {
        let operation_id = crate::stable_event_uuid(
            "sigil-application-operation-v1",
            &format!("{session_scope_id}:{reservation_key_digest}:{fingerprint}"),
        );
        let binding = Self {
            schema_version: 1,
            session_scope_id,
            domain_session_scope_id: None,
            reservation_key_digest,
            fingerprint,
            operation_id,
            target,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn domain_session_scope_id(&self) -> &str {
        self.domain_session_scope_id
            .as_deref()
            .unwrap_or(&self.session_scope_id)
    }

    pub fn validate(&self) -> Result<()> {
        let digest =
            |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if self.schema_version != 1
            || self.session_scope_id.is_empty()
            || self
                .domain_session_scope_id
                .as_ref()
                .is_some_and(|scope| scope.is_empty() || scope.len() > 256)
            || !digest(&self.reservation_key_digest)
            || !digest(&self.fingerprint)
            || self.operation_id
                != crate::stable_event_uuid(
                    "sigil-application-operation-v1",
                    &format!(
                        "{}:{}:{}",
                        self.session_scope_id, self.reservation_key_digest, self.fingerprint
                    ),
                )
        {
            bail!("application operation binding is invalid");
        }
        if serde_json::to_vec(&self.target)?.len() > 8192 {
            bail!("application operation target exceeds its bound");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationOperationEvidenceV1 {
    pub event_id: String,
    pub payload_digest: String,
}

/// This marker is written in the same crash-safe bundle as every referenced domain event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationOperationCommittedV1 {
    pub binding: ApplicationOperationBindingV1,
    pub domain_events: Vec<ApplicationOperationEvidenceV1>,
}

impl ApplicationOperationCommittedV1 {
    pub fn validate(&self) -> Result<()> {
        self.binding.validate()?;
        if self.domain_events.is_empty() || self.domain_events.len() > 1024 {
            bail!("application operation requires a bounded nonempty domain batch");
        }
        let mut seen = std::collections::BTreeSet::new();
        for event in &self.domain_events {
            if event.event_id.is_empty()
                || event.payload_digest.len() != 64
                || !event
                    .payload_digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
                || !seen.insert(&event.event_id)
            {
                bail!("application operation evidence identity is invalid");
            }
        }
        Ok(())
    }
}
