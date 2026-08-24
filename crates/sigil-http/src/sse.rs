use std::{collections::BTreeMap, sync::Mutex};

use serde::{Deserialize, Serialize};
use sigil_kernel::{
    PublicRunEvent, PublicRunEventKind, ToolCall, ToolResultStatus, safe_persistence_json_value,
    safe_persistence_text,
};
use sigil_runtime::conversation_display::{
    ConversationLiveProvisionalSlotV1, conversation_live_provisional_id,
};
use thiserror::Error as ThisError;
use tokio::sync::broadcast;

use crate::HttpPendingApproval;
use crate::journal::HttpDurableProtocolJournal;

/// SSE event name used for public run events.
pub const HTTP_RUN_EVENT_SSE_NAME: &str = "run_event";
/// Current schema version for HTTP protocol event envelopes.
pub const HTTP_PROTOCOL_EVENT_SCHEMA_VERSION: u32 = 2;

const HTTP_PROTOCOL_CURSOR_PREFIX: &str = "sigil-http-run-v1";

/// One Server-Sent Events frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpSseEvent {
    id: Option<String>,
    event: String,
    data: String,
}

impl HttpSseEvent {
    /// Creates one SSE frame payload.
    ///
    /// # Errors
    ///
    /// Returns an error when the event name is empty or contains line breaks.
    pub fn new(event: impl Into<String>, data: impl Into<String>) -> Result<Self, HttpSseError> {
        Self::with_id(None, event, data)
    }

    /// Creates one SSE frame payload with an optional `id:` cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the event name or id is empty or contains line breaks.
    pub fn with_id(
        id: Option<String>,
        event: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<Self, HttpSseError> {
        let event = event.into();
        if event.is_empty() || event.contains('\r') || event.contains('\n') {
            return Err(HttpSseError::InvalidEventName { event });
        }
        if let Some(id) = id.as_deref()
            && (id.trim().is_empty() || id.contains('\r') || id.contains('\n'))
        {
            return Err(HttpSseError::InvalidEventId { id: id.to_owned() });
        }
        Ok(Self {
            id,
            event,
            data: data.into(),
        })
    }

    /// Returns the optional SSE event id.
    #[must_use]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Returns the SSE event name.
    #[must_use]
    pub fn event(&self) -> &str {
        &self.event
    }

    /// Returns the serialized SSE data payload.
    #[must_use]
    pub fn data(&self) -> &str {
        &self.data
    }

    /// Encodes the frame using SSE `event:` and `data:` fields.
    #[must_use]
    pub fn encode(&self) -> String {
        let mut encoded = String::new();
        if let Some(id) = &self.id {
            append_sse_field(&mut encoded, "id", id);
        }
        append_sse_field(&mut encoded, "event", &self.event);
        append_sse_field(&mut encoded, "data", &self.data);
        encoded.push('\n');
        encoded
    }
}

/// Errors returned while serializing HTTP SSE frames.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpSseError {
    /// The SSE event name is invalid.
    #[error("http sse event name is invalid: {event}")]
    InvalidEventName { event: String },
    /// The SSE event id is invalid.
    #[error("http sse event id is invalid: {id}")]
    InvalidEventId { id: String },
    /// The public run event could not be serialized to JSON.
    #[error("http run event serialization failed: {message}")]
    Serialize { message: String },
    /// A durable protocol cursor could not be generated.
    #[error("http protocol cursor is invalid: {message}")]
    Cursor { message: String },
}

/// Public replay class for HTTP protocol events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProtocolEventClass {
    /// Replayable event derived from a durable or recovery-relevant fact.
    Durable,
    /// Process-local progress event that is not replayed after reconnect.
    Transient,
}

/// HTTP-facing protocol event envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpProtocolEvent {
    /// Protocol envelope schema version.
    pub schema_version: u32,
    /// Whether clients can expect this event to replay after reconnect.
    pub event_class: HttpProtocolEventClass,
    /// SSE `id:` value for durable events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_id: Option<String>,
    /// Guard material required to resolve an HTTP-owned approval request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_request: Option<HttpPendingApproval>,
    /// Opaque identity for an exact live semantic slot that a durable display item may reconcile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional_id: Option<String>,
    /// Public run event payload.
    pub run_event: PublicRunEvent,
}

impl HttpProtocolEvent {
    /// Wraps one public run event in the HTTP protocol envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable cursor cannot be generated for the event.
    pub fn from_run_event(mut event: PublicRunEvent) -> Result<Self, HttpProtocolCursorError> {
        let event_class = protocol_event_class(&event.event);
        let provisional_id = protocol_provisional_id(&event)?;
        if event_class == HttpProtocolEventClass::Durable {
            project_durable_text_for_persistence(&mut event.event);
        }
        let replay_id = match event_class {
            HttpProtocolEventClass::Durable => {
                Some(HttpProtocolCursor::from_run_event(&event)?.encode())
            }
            HttpProtocolEventClass::Transient => None,
        };
        Ok(Self {
            schema_version: HTTP_PROTOCOL_EVENT_SCHEMA_VERSION,
            event_class,
            replay_id,
            approval_request: None,
            provisional_id,
            run_event: event,
        })
    }

    /// Returns whether this protocol event is replayable after reconnect.
    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.event_class == HttpProtocolEventClass::Durable
    }

    /// Returns a DTO view that separates durable replayable events from transient live events.
    #[must_use]
    pub fn view(&self) -> HttpProtocolEventView {
        match self.event_class {
            HttpProtocolEventClass::Durable => {
                HttpProtocolEventView::Durable(Box::new(HttpDurableEventView {
                    schema_version: self.schema_version,
                    replay_id: self.replay_id.clone().unwrap_or_default(),
                    approval_request: self.approval_request.clone(),
                    provisional_id: self.provisional_id.clone(),
                    run_event: self.run_event.clone(),
                }))
            }
            HttpProtocolEventClass::Transient => {
                HttpProtocolEventView::Transient(Box::new(HttpTransientEventView {
                    schema_version: self.schema_version,
                    provisional_id: self.provisional_id.clone(),
                    run_event: self.run_event.clone(),
                }))
            }
        }
    }

    pub(crate) fn has_valid_approval_metadata(&self) -> bool {
        match (&self.approval_request, &self.run_event.event) {
            (None, _) => true,
            (
                Some(approval),
                PublicRunEventKind::ApprovalRequested {
                    approval_identity,
                    call,
                    spec,
                    ..
                },
            ) => {
                self.is_durable()
                    && approval.call_id == call.id
                    && approval.tool_name == spec.name
                    && approval.approval_request_id == approval_identity.approval_request_id
                    && approval.policy_version == approval_identity.policy_version
                    && approval.expires_at_ms == approval_identity.expires_at_ms
                    && approval_identity.session_id == self.run_event.session_id
                    && approval_identity.run_id == self.run_event.run_id
                    && approval_identity.call_id == call.id
                    && approval_guard_is_persistence_safe(approval)
                    && approval.display.event_sequence == self.run_event.sequence
            }
            (Some(_), _) => false,
        }
    }
}

fn protocol_provisional_id(
    event: &PublicRunEvent,
) -> Result<Option<String>, HttpProtocolCursorError> {
    let slot = match &event.event {
        PublicRunEventKind::RunStarted { .. } => Some(ConversationLiveProvisionalSlotV1::User),
        PublicRunEventKind::AssistantMessage { message } => {
            Some(ConversationLiveProvisionalSlotV1::AssistantMessage {
                message_id: message.id.clone(),
            })
        }
        PublicRunEventKind::ConversationRouteChanged { .. }
        | PublicRunEventKind::PlanReviewChanged { .. } => None,
        PublicRunEventKind::ToolCallStarted { call }
        | PublicRunEventKind::ToolCallCompleted { call }
        | PublicRunEventKind::ApprovalRequested { call, .. } => {
            let slot = if matches!(&event.event, PublicRunEventKind::ApprovalRequested { .. }) {
                ConversationLiveProvisionalSlotV1::Approval {
                    call_id: call.id.clone(),
                }
            } else {
                ConversationLiveProvisionalSlotV1::Tool {
                    call_id: call.id.clone(),
                }
            };
            Some(slot)
        }
        PublicRunEventKind::ToolCallArgsDelta { id, .. } => {
            Some(ConversationLiveProvisionalSlotV1::Tool {
                call_id: id.clone(),
            })
        }
        PublicRunEventKind::ToolResult { result } => {
            Some(ConversationLiveProvisionalSlotV1::Tool {
                call_id: result.call_id.clone(),
            })
        }
        PublicRunEventKind::ToolProgress { progress } => {
            Some(ConversationLiveProvisionalSlotV1::Tool {
                call_id: progress.call_id.clone(),
            })
        }
        PublicRunEventKind::ApprovalResolved { call_id, .. } => {
            Some(ConversationLiveProvisionalSlotV1::Approval {
                call_id: call_id.clone(),
            })
        }
        PublicRunEventKind::RunFinished { .. }
        | PublicRunEventKind::RunAwaitingUserInput { .. }
        | PublicRunEventKind::RunFailed { .. }
        | PublicRunEventKind::RunBlocked { .. }
        | PublicRunEventKind::RunPaused { .. }
        | PublicRunEventKind::RunInterrupted { .. }
        | PublicRunEventKind::RouteRecoveryRequired { .. }
        | PublicRunEventKind::RunCancelled => Some(ConversationLiveProvisionalSlotV1::Terminal),
        PublicRunEventKind::RouteTransition { .. }
        | PublicRunEventKind::TaskRunStarted { .. }
        | PublicRunEventKind::TaskRunFinished { .. }
        | PublicRunEventKind::TaskRoutingChanged { .. }
        | PublicRunEventKind::TaskPhaseChanged { .. }
        | PublicRunEventKind::TaskExecutionAdmitted { .. }
        | PublicRunEventKind::TaskPlanUpdated { .. }
        | PublicRunEventKind::TaskChecklistUpdated { .. }
        | PublicRunEventKind::TaskBatchChanged { .. }
        | PublicRunEventKind::TaskStepChanged { .. }
        | PublicRunEventKind::IntegrationLaneChanged { .. }
        | PublicRunEventKind::ProviderTurnRecoveryChanged { .. }
        | PublicRunEventKind::ProviderTurnPartialOutputDiscarded { .. }
        | PublicRunEventKind::UserInputChanged { .. }
        | PublicRunEventKind::TextDelta { .. }
        | PublicRunEventKind::ReasoningDelta { .. }
        | PublicRunEventKind::Usage { .. }
        | PublicRunEventKind::ContinuationState { .. }
        | PublicRunEventKind::TerminalLifecycle { .. }
        | PublicRunEventKind::Control { .. }
        | PublicRunEventKind::Notice { .. } => None,
    };
    slot.map(|slot| conversation_live_provisional_id(&event.session_id, &event.run_id, &slot))
        .transpose()
        .map_err(|_| HttpProtocolCursorError::InvalidProvisionalIdentity)
}

fn approval_guard_is_persistence_safe(approval: &HttpPendingApproval) -> bool {
    approval.expires_at_ms > 0
        && !approval.policy_version.is_empty()
        && approval.policy_version.len() <= 256
        && safe_persistence_text(&approval.policy_version) == approval.policy_version
        && !approval.approval_request_id.is_empty()
        && approval.approval_request_id.len() <= 256
        && safe_persistence_text(&approval.approval_request_id) == approval.approval_request_id
        && is_lower_hex_sha256(&approval.tool_call_hash)
        && safe_persistence_text(&approval.call_id) == approval.call_id
        && safe_persistence_text(&approval.tool_name) == approval.tool_name
        && approval_display_is_persistence_safe(&approval.display)
}

fn approval_display_is_persistence_safe(display: &crate::HttpPendingApprovalDisplay) -> bool {
    display.event_sequence > 0
        && display.effects.len() <= 16
        && display.subjects.len() <= 16
        && display.analysis_reason_codes.len() <= 8
        && display.analysis_reasons.len() <= 8
        && display.containment.len() <= 8
        && display.decision_reasons.len() <= 8
        && safe_bounded_approval_fact(&display.analysis_status)
        && safe_bounded_approval_fact(&display.safe_summary_title)
        && safe_bounded_approval_fact(&display.safe_summary_detail)
        && display
            .effects
            .iter()
            .all(|value| safe_bounded_approval_fact(value))
        && display
            .analysis_reason_codes
            .iter()
            .all(|value| safe_bounded_approval_fact(value))
        && display
            .analysis_reasons
            .iter()
            .all(|value| safe_bounded_approval_fact(value))
        && display
            .containment
            .iter()
            .all(|value| safe_bounded_approval_fact(value))
        && display
            .decision_reasons
            .iter()
            .all(|value| safe_bounded_approval_fact(value))
        && display
            .operation
            .as_deref()
            .is_none_or(safe_bounded_approval_fact)
        && display
            .risk
            .as_deref()
            .is_none_or(safe_bounded_approval_fact)
        && display.subjects.iter().all(|subject| {
            safe_bounded_approval_fact(&subject.kind)
                && safe_bounded_approval_fact(&subject.scope)
                && subject.workspace_label.as_deref().is_none_or(|label| {
                    safe_bounded_approval_fact(label)
                        && !std::path::Path::new(label).is_absolute()
                        && !std::path::Path::new(label)
                            .components()
                            .any(|component| matches!(component, std::path::Component::ParentDir))
                })
        })
}

fn safe_bounded_approval_fact(value: &str) -> bool {
    value.len() <= 2_048 && safe_persistence_text(value) == value
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn project_durable_text_for_persistence(event: &mut PublicRunEventKind) {
    match event {
        PublicRunEventKind::RouteTransition { transition } => {
            transition.connection_id = transition
                .connection_id
                .as_deref()
                .map(safe_persistence_text);
            transition.model_id = transition.model_id.as_deref().map(safe_persistence_text);
        }
        PublicRunEventKind::RunStarted { prompt } => {
            *prompt = safe_persistence_text(prompt);
        }
        PublicRunEventKind::TaskRunStarted { objective, .. } => {
            *objective = safe_persistence_text(objective);
        }
        PublicRunEventKind::RunFinished { final_text } => {
            *final_text = safe_persistence_text(final_text);
        }
        PublicRunEventKind::TaskRunFinished { status, .. } => {
            *status = safe_persistence_text(status);
        }
        PublicRunEventKind::TaskRoutingChanged {
            handoff_id,
            status,
            task_id,
        } => {
            *handoff_id = safe_persistence_text(handoff_id);
            *status = safe_persistence_text(status);
            *task_id = task_id.as_deref().map(safe_persistence_text);
        }
        PublicRunEventKind::ConversationRouteChanged {
            decision_id,
            status,
            ..
        } => {
            *decision_id = safe_persistence_text(decision_id);
            *status = safe_persistence_text(status);
        }
        PublicRunEventKind::PlanReviewChanged {
            plan_review_id,
            plan_id,
            ..
        } => {
            *plan_review_id = safe_persistence_text(plan_review_id);
            *plan_id = safe_persistence_text(plan_id);
        }
        PublicRunEventKind::UserInputChanged { request, .. } => {
            request.prompt = safe_persistence_text(&request.prompt);
            for question in &mut request.questions {
                question.id = safe_persistence_text(&question.id);
                question.header = safe_persistence_text(&question.header);
                question.question = safe_persistence_text(&question.question);
                question.description = question.description.as_deref().map(safe_persistence_text);
                let options = match &mut question.field {
                    sigil_kernel::UserInputFieldKindV1::SingleSelect { options, .. }
                    | sigil_kernel::UserInputFieldKindV1::MultiSelect { options, .. } => {
                        Some(options)
                    }
                    _ => None,
                };
                if let Some(options) = options {
                    for option in options {
                        option.id = safe_persistence_text(&option.id);
                        option.label = safe_persistence_text(&option.label);
                        option.description =
                            option.description.as_deref().map(safe_persistence_text);
                    }
                }
            }
        }
        PublicRunEventKind::TaskPhaseChanged {
            task_id, status, ..
        } => {
            *task_id = task_id.as_deref().map(safe_persistence_text);
            *status = safe_persistence_text(status);
        }
        PublicRunEventKind::TaskExecutionAdmitted { task_id, execution } => {
            *task_id = safe_persistence_text(task_id);
            if let sigil_kernel::TaskExecutionBindingV1::Direct { admission_id } = execution {
                *admission_id = safe_persistence_text(admission_id);
            }
        }
        PublicRunEventKind::TaskPlanUpdated {
            task_id,
            status,
            steps,
            ..
        } => {
            *task_id = safe_persistence_text(task_id);
            *status = safe_persistence_text(status);
            for step in steps {
                step.step_id = safe_persistence_text(&step.step_id);
                step.title = safe_persistence_text(&step.title);
                step.role = safe_persistence_text(&step.role);
                step.depends_on = step
                    .depends_on
                    .iter()
                    .map(|dependency| safe_persistence_text(dependency))
                    .collect();
                step.mode = safe_persistence_text(&step.mode);
                step.isolation = safe_persistence_text(&step.isolation);
            }
        }
        PublicRunEventKind::TaskChecklistUpdated { task_id, items, .. } => {
            *task_id = safe_persistence_text(task_id);
            for item in items {
                item.item_id = safe_persistence_text(&item.item_id);
                item.text = safe_persistence_text(&item.text);
            }
        }
        PublicRunEventKind::TaskBatchChanged {
            task_id, batch_id, ..
        } => {
            *task_id = safe_persistence_text(task_id);
            *batch_id = safe_persistence_text(batch_id);
        }
        PublicRunEventKind::TaskStepChanged {
            task_id,
            step_id,
            attempt_id,
            status,
            ..
        } => {
            *task_id = safe_persistence_text(task_id);
            *step_id = safe_persistence_text(step_id);
            *attempt_id = attempt_id.as_deref().map(safe_persistence_text);
            *status = safe_persistence_text(status);
        }
        PublicRunEventKind::IntegrationLaneChanged {
            task_id,
            plan_id,
            lane_id,
            status,
            conflicts,
            ..
        } => {
            *task_id = safe_persistence_text(task_id);
            *plan_id = safe_persistence_text(plan_id);
            *lane_id = safe_persistence_text(lane_id);
            *status = safe_persistence_text(status);
            *conflicts = conflicts
                .iter()
                .map(|conflict| safe_persistence_text(conflict))
                .collect();
        }
        PublicRunEventKind::RunFailed { error } => {
            *error = safe_persistence_text(error);
        }
        PublicRunEventKind::RunBlocked { reason }
        | PublicRunEventKind::RunPaused { reason }
        | PublicRunEventKind::RunInterrupted { reason } => {
            *reason = safe_persistence_text(reason);
        }
        PublicRunEventKind::ProviderTurnRecoveryChanged { recovery } => {
            recovery.reason_code = recovery.reason_code.as_deref().map(safe_persistence_text);
        }
        PublicRunEventKind::ProviderTurnPartialOutputDiscarded { .. } => {}
        PublicRunEventKind::ApprovalResolved { reason, .. } => {
            if let Some(reason) = reason {
                *reason = safe_persistence_text(reason);
            }
        }
        PublicRunEventKind::AssistantMessage { message } => {
            if let Some(content) = &mut message.content {
                *content = safe_persistence_text(content);
            }
            for call in &mut message.tool_calls {
                project_tool_call_for_http_persistence(call);
            }
        }
        PublicRunEventKind::Notice { message } => {
            *message = safe_persistence_text(message);
        }
        PublicRunEventKind::ToolCallStarted { call }
        | PublicRunEventKind::ToolCallCompleted { call } => {
            project_tool_call_for_http_persistence(call);
        }
        PublicRunEventKind::ApprovalRequested {
            analysis,
            call,
            command_permission_matches,
            confirmation,
            decision_reasons,
            safe_summary,
            spec,
            subjects,
            preview,
            ..
        } => {
            project_tool_call_for_http_persistence(call);
            spec.description = safe_persistence_text(&spec.description);
            spec.input_schema = safe_persistence_json_value(std::mem::take(&mut spec.input_schema));
            safe_summary.title = safe_persistence_text(&safe_summary.title);
            safe_summary.detail = safe_persistence_text(&safe_summary.detail);
            for reason in decision_reasons {
                reason.code = safe_persistence_text(&reason.code);
                reason.detail = safe_persistence_text(&reason.detail);
            }
            match analysis {
                sigil_kernel::ToolAnalysisStatus::Complete => {}
                sigil_kernel::ToolAnalysisStatus::Conservative { reasons } => {
                    for reason in reasons {
                        reason.detail = reason.detail.as_deref().map(safe_persistence_text);
                    }
                }
                sigil_kernel::ToolAnalysisStatus::Unsupported { reason }
                | sigil_kernel::ToolAnalysisStatus::Invalid { reason } => {
                    reason.detail = reason.detail.as_deref().map(safe_persistence_text);
                }
            }
            for subject in subjects {
                subject.original = safe_persistence_text(&subject.original);
                subject.normalized = safe_persistence_text(&subject.normalized);
                if let Some(path) = &mut subject.canonical_path {
                    *path = safe_persistence_text(&path.to_string_lossy()).into();
                }
            }
            for matched in command_permission_matches {
                matched.pattern = safe_persistence_text(&matched.pattern);
                matched.command = safe_persistence_text(&matched.command);
            }
            if let Some(sigil_kernel::PermissionConfirmation::TypePhrase { phrase }) = confirmation
            {
                *phrase = safe_persistence_text(phrase);
            }
            if let Some(preview) = preview {
                preview.title = safe_persistence_text(&preview.title);
                preview.summary = safe_persistence_text(&preview.summary);
                preview.body = safe_persistence_text(&preview.body);
                preview.changed_files = preview
                    .changed_files
                    .iter()
                    .map(|path| safe_persistence_text(path))
                    .collect();
                for file in &mut preview.file_diffs {
                    file.path = safe_persistence_text(&file.path);
                    file.diff = safe_persistence_text(&file.diff);
                }
            }
        }
        PublicRunEventKind::ToolResult { result } => {
            result.content = safe_persistence_text(&result.content);
            result.metadata.changed_files = result
                .metadata
                .changed_files
                .iter()
                .map(|path| safe_persistence_text(path))
                .collect();
            result.metadata.details =
                safe_persistence_json_value(std::mem::take(&mut result.metadata.details));
            if let Some(receipt) = &mut result.metadata.receipt {
                if let Some(key) = &mut receipt.idempotency_key {
                    *key = safe_persistence_text(key);
                }
                receipt.mutation_operation_ids = receipt
                    .mutation_operation_ids
                    .iter()
                    .map(|id| safe_persistence_text(id))
                    .collect();
            }
            if let ToolResultStatus::Error(error) = &mut result.status {
                error.message = safe_persistence_text(&error.message);
                error.details = safe_persistence_json_value(std::mem::take(&mut error.details));
            }
        }
        PublicRunEventKind::ContinuationState { state } => {
            state.provider_name = safe_persistence_text(&state.provider_name);
            state.state_kind = safe_persistence_text(&state.state_kind);
            if let Some(message_id) = &mut state.message_id {
                *message_id = safe_persistence_text(message_id);
            }
            state.opaque_blob = serde_json::json!({
                "projection": "omitted_from_http_durable_event"
            });
        }
        PublicRunEventKind::Control { control } => {
            control.kind = safe_persistence_text(&control.kind);
            control.payload = None;
        }
        PublicRunEventKind::TerminalLifecycle { .. } => {}
        PublicRunEventKind::RunCancelled
        | PublicRunEventKind::RunAwaitingUserInput { .. }
        | PublicRunEventKind::RouteRecoveryRequired { .. }
        | PublicRunEventKind::TextDelta { .. }
        | PublicRunEventKind::ReasoningDelta { .. }
        | PublicRunEventKind::ToolCallArgsDelta { .. }
        | PublicRunEventKind::ToolProgress { .. }
        | PublicRunEventKind::Usage { .. } => {}
    }
}

fn project_tool_call_for_http_persistence(call: &mut ToolCall) {
    call.args_json = serde_json::from_str(&call.args_json).map_or_else(
        |_| {
            serde_json::json!({
                "projection": "malformed_arguments",
                "raw_bytes": call.args_json.len(),
            })
            .to_string()
        },
        |value| safe_persistence_json_value(value).to_string(),
    );
}

/// Explicit durable/transient event view used by future protocol clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event_class")]
pub enum HttpProtocolEventView {
    Durable(Box<HttpDurableEventView>),
    Transient(Box<HttpTransientEventView>),
}

/// Replayable event view with a cursor suitable for SSE `Last-Event-ID`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpDurableEventView {
    pub schema_version: u32,
    pub replay_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_request: Option<HttpPendingApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional_id: Option<String>,
    pub run_event: PublicRunEvent,
}

/// Process-local event view that is not replayable after reconnect.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpTransientEventView {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisional_id: Option<String>,
    pub run_event: PublicRunEvent,
}

/// Durable HTTP replay cursor carried in SSE `id:` and `Last-Event-ID`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct HttpProtocolCursor {
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
}

impl HttpProtocolCursor {
    /// Builds a cursor for one public run event.
    ///
    /// # Errors
    ///
    /// Returns an error when a component cannot be encoded safely in an SSE id.
    pub fn from_run_event(event: &PublicRunEvent) -> Result<Self, HttpProtocolCursorError> {
        validate_cursor_component("session_id", &event.session_id)?;
        validate_cursor_component("run_id", &event.run_id)?;
        if event.sequence == 0 {
            return Err(HttpProtocolCursorError::InvalidSequence { sequence: 0 });
        }
        Ok(Self {
            session_id: event.session_id.clone(),
            run_id: event.run_id.clone(),
            sequence: event.sequence,
        })
    }

    /// Encodes this cursor for SSE `id:` / `Last-Event-ID`.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{HTTP_PROTOCOL_CURSOR_PREFIX}:{}:{}:{}",
            self.session_id, self.run_id, self.sequence
        )
    }

    /// Parses an SSE `Last-Event-ID` cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is malformed or uses another cursor version.
    pub fn parse(value: &str) -> Result<Self, HttpProtocolCursorError> {
        let parts = value.split(':').collect::<Vec<_>>();
        if parts.len() != 4 || parts[0] != HTTP_PROTOCOL_CURSOR_PREFIX {
            return Err(HttpProtocolCursorError::InvalidFormat {
                cursor: value.to_owned(),
            });
        }
        validate_cursor_component("session_id", parts[1])?;
        validate_cursor_component("run_id", parts[2])?;
        let sequence =
            parts[3]
                .parse::<u64>()
                .map_err(|_| HttpProtocolCursorError::InvalidFormat {
                    cursor: value.to_owned(),
                })?;
        if sequence == 0 {
            return Err(HttpProtocolCursorError::InvalidSequence { sequence });
        }
        Ok(Self {
            session_id: parts[1].to_owned(),
            run_id: parts[2].to_owned(),
            sequence,
        })
    }
}

/// Cursor parsing and encoding errors.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolCursorError {
    /// Cursor does not match the HTTP protocol cursor format.
    #[error("invalid cursor format: {cursor}")]
    InvalidFormat { cursor: String },
    /// Cursor component cannot be represented safely inside an SSE id.
    #[error("invalid cursor component {component}: {value}")]
    InvalidComponent {
        component: &'static str,
        value: String,
    },
    /// Cursor sequence must be positive.
    #[error("invalid cursor sequence: {sequence}")]
    InvalidSequence { sequence: u64 },
    /// The event's stable semantic slot could not produce an opaque live identity.
    #[error("invalid live provisional identity")]
    InvalidProvisionalIdentity,
}

/// Errors returned while replaying durable HTTP protocol events.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpProtocolReplayError {
    /// The provided cursor could not be parsed.
    #[error("http protocol replay cursor is invalid: {message}")]
    InvalidCursor { message: String },
    /// The cursor belongs to another session/run stream.
    #[error("http protocol replay cursor scope mismatch")]
    CursorScopeMismatch,
    /// The cursor is newer than the buffered run stream.
    #[error("http protocol replay cursor is ahead of buffered events")]
    CursorAhead,
    /// The cursor refers to durable history older than the bounded retained suffix.
    #[error("http protocol replay cursor has expired from retained history")]
    CursorExpired,
    /// Durable replay storage could not be read safely.
    #[error("http protocol replay journal is unavailable")]
    JournalUnavailable,
}

/// Errors returned while durably publishing an HTTP protocol event.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpEventPublishError {
    /// The public event could not produce a stable replay cursor.
    #[error("http protocol event cursor is invalid: {message}")]
    Cursor { message: String },
    /// The durable event could not be committed to the production replay journal.
    #[error("http protocol event journal rejected publication: {message}")]
    Journal { message: String },
    /// HTTP approval guard material did not match the public approval event.
    #[error("http protocol approval metadata does not match its run event")]
    ApprovalMetadata,
    /// The adapter could not register approval routing under the allocated event sequence.
    #[error("http protocol approval registration failed: {message}")]
    ApprovalRegistration { message: String },
}

/// Errors returned while receiving a transient live event.
#[derive(Debug, Clone, PartialEq, Eq, ThisError)]
pub enum HttpLiveEventRecvError {
    /// The subscriber lagged behind the bounded channel and one or more live events were dropped.
    #[error("http live event subscriber lagged and dropped {dropped} events")]
    Lagged { dropped: u64 },
    /// The live event bus was closed.
    #[error("http live event stream is closed")]
    Closed,
}

/// In-memory protocol event buffer used by HTTP/SSE adapters.
///
/// The buffer stores both durable and transient views for current subscribers, but reconnect replay
/// only returns durable events whose sequence is newer than the provided `Last-Event-ID` cursor.
#[derive(Default)]
pub struct HttpProtocolEventBuffer {
    events: Mutex<Vec<HttpProtocolEvent>>,
}

impl HttpProtocolEventBuffer {
    /// Creates an empty protocol event buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one public run event and returns the stored protocol envelope.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable cursor cannot be generated.
    pub fn push_run_event(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpProtocolCursorError> {
        let event = HttpProtocolEvent::from_run_event(event)?;
        self.events
            .lock()
            .expect("http protocol event buffer lock should not be poisoned")
            .push(event.clone());
        Ok(event)
    }

    /// Replays durable events for one run after an optional `Last-Event-ID` cursor.
    ///
    /// Transient protocol events are intentionally filtered out. A cursor from another run fails
    /// closed so clients cannot accidentally stitch together unrelated event streams.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is malformed, belongs to another stream, or is ahead of the
    /// buffered stream.
    pub fn replay_run_after(
        &self,
        session_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Vec<HttpProtocolEvent>, HttpProtocolReplayError> {
        let cursor = match last_event_id {
            Some(value) => Some(HttpProtocolCursor::parse(value).map_err(|error| {
                HttpProtocolReplayError::InvalidCursor {
                    message: error.to_string(),
                }
            })?),
            None => None,
        };
        if let Some(cursor) = &cursor
            && (cursor.session_id != session_id || cursor.run_id != run_id)
        {
            return Err(HttpProtocolReplayError::CursorScopeMismatch);
        }
        let after_sequence = cursor.as_ref().map_or(0, |cursor| cursor.sequence);
        let events = self
            .events
            .lock()
            .expect("http protocol event buffer lock should not be poisoned");
        let latest_sequence = events
            .iter()
            .filter(|event| {
                event.run_event.session_id == session_id && event.run_event.run_id == run_id
            })
            .map(|event| event.run_event.sequence)
            .max()
            .unwrap_or(0);
        if after_sequence > latest_sequence {
            return Err(HttpProtocolReplayError::CursorAhead);
        }
        Ok(events
            .iter()
            .filter(|event| {
                event.is_durable()
                    && event.run_event.session_id == session_id
                    && event.run_event.run_id == run_id
                    && event.run_event.sequence > after_sequence
            })
            .cloned()
            .collect())
    }

    fn latest_run_sequence(&self, session_id: &str, run_id: &str) -> Option<u64> {
        self.events
            .lock()
            .expect("http protocol event buffer lock should not be poisoned")
            .iter()
            .filter(|event| {
                event.run_event.session_id == session_id && event.run_event.run_id == run_id
            })
            .map(|event| event.run_event.sequence)
            .max()
    }
}

/// Bounded live event bus for local clients.
///
/// The bus broadcasts both durable and transient protocol events to active subscribers. Durable
/// replay comes from the configured journal in production, while synthetic adapters retain an
/// in-memory replay buffer. Lagged transient delivery is reported as a live-stream drop and never
/// mutates durable replay semantics.
pub struct HttpLiveEventBus {
    buffer: HttpProtocolEventBuffer,
    durable_journal: Option<std::sync::Arc<HttpDurableProtocolJournal>>,
    publication_lock: Mutex<()>,
    latest_sequences: Mutex<BTreeMap<HttpRunSequenceKey, u64>>,
    sender: broadcast::Sender<HttpLiveBusMessage>,
}

#[derive(Debug, Clone)]
enum HttpLiveBusMessage {
    Event(Box<HttpProtocolEvent>),
    StreamClosed { session_id: String, run_id: String },
}

impl HttpLiveEventBus {
    /// Creates a live bus with bounded subscriber capacity.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity);
        Self {
            buffer: HttpProtocolEventBuffer::new(),
            durable_journal: None,
            publication_lock: Mutex::new(()),
            latest_sequences: Mutex::new(BTreeMap::new()),
            sender,
        }
    }

    /// Creates a live bus backed by a restart-safe durable replay journal.
    #[must_use]
    pub fn with_durable_journal(
        capacity: usize,
        journal: std::sync::Arc<HttpDurableProtocolJournal>,
    ) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity);
        Self {
            buffer: HttpProtocolEventBuffer::new(),
            durable_journal: Some(journal),
            publication_lock: Mutex::new(()),
            latest_sequences: Mutex::new(BTreeMap::new()),
            sender,
        }
    }

    /// Subscribes to live protocol events from this point forward.
    #[must_use]
    pub fn subscribe(&self) -> HttpLiveEventSubscriber {
        HttpLiveEventSubscriber {
            receiver: self.sender.subscribe(),
        }
    }

    /// Returns whether durable replay is configured for every durable publication.
    #[must_use]
    pub fn has_durable_journal(&self) -> bool {
        self.durable_journal.is_some()
    }

    pub(crate) fn attach_managed_protocol_replay(
        &self,
        writer: std::sync::Arc<
            sigil_runtime::managed_storage_writer::ManagedStorageWriterAdapterV1,
        >,
        key: &str,
    ) -> Result<(), HttpEventPublishError> {
        let Some(journal) = self.durable_journal.as_ref() else {
            return Err(HttpEventPublishError::Journal {
                message: "managed protocol replay requires a durable journal".to_owned(),
            });
        };
        journal
            .attach_managed_writer(writer, key)
            .map_err(|error| HttpEventPublishError::Journal {
                message: error.to_string(),
            })
    }

    /// Records one run event and broadcasts it to active subscribers.
    ///
    /// # Errors
    ///
    /// Returns an error when a durable cursor cannot be generated for the event.
    pub fn publish_run_event(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_run_event_with_policy(event, None, false, false)
    }

    /// Publishes a foreground terminal while retaining the stream for owned terminal tasks.
    pub fn publish_run_event_with_stream_continuation(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_run_event_with_policy(event, None, true, false)
    }

    /// Publishes the final terminal-task lifecycle and closes a retained stream atomically in the
    /// durable journal before notifying live subscribers.
    pub fn publish_run_event_and_close_stream(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_run_event_with_policy(event, None, false, true)
    }

    /// Publishes a run event with adapter-owned guard material for an approval request.
    ///
    /// # Errors
    ///
    /// Returns an error when the guard does not match the public approval event or durable
    /// publication fails.
    pub fn publish_run_event_with_approval(
        &self,
        event: PublicRunEvent,
        approval_request: Option<HttpPendingApproval>,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_run_event_with_policy(event, approval_request, false, false)
    }

    fn publish_run_event_with_policy(
        &self,
        event: PublicRunEvent,
        approval_request: Option<HttpPendingApproval>,
        keep_stream_open: bool,
        close_stream_after_event: bool,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        let _publication =
            self.publication_lock
                .lock()
                .map_err(|_| HttpEventPublishError::Journal {
                    message: "http event publication sequencer is unavailable".to_owned(),
                })?;
        self.publish_run_event_with_policy_locked(
            event,
            approval_request,
            keep_stream_open,
            close_stream_after_event,
        )
    }

    fn publish_run_event_with_policy_locked(
        &self,
        event: PublicRunEvent,
        approval_request: Option<HttpPendingApproval>,
        keep_stream_open: bool,
        close_stream_after_event: bool,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        let mut event = HttpProtocolEvent::from_run_event(event).map_err(|error| {
            HttpEventPublishError::Cursor {
                message: error.to_string(),
            }
        })?;
        event.approval_request = approval_request;
        if !event.has_valid_approval_metadata() {
            return Err(HttpEventPublishError::ApprovalMetadata);
        }
        if event.is_durable()
            && let Some(journal) = &self.durable_journal
        {
            let result = if close_stream_after_event {
                journal.append_and_close_stream(event.clone())
            } else {
                journal.append_with_stream_continuation(event.clone(), keep_stream_open)
            };
            result.map_err(|error| HttpEventPublishError::Journal {
                message: error.to_string(),
            })?;
        }
        if self.durable_journal.is_none() {
            self.buffer
                .events
                .lock()
                .expect("http protocol event buffer lock should not be poisoned")
                .push(event.clone());
        }
        let sequence_key = HttpRunSequenceKey {
            session_id: event.run_event.session_id.clone(),
            run_id: event.run_event.run_id.clone(),
        };
        let terminal = matches!(
            &event.run_event.event,
            PublicRunEventKind::RunFinished { .. }
                | PublicRunEventKind::RunFailed { .. }
                | PublicRunEventKind::RunBlocked { .. }
                | PublicRunEventKind::RunPaused { .. }
                | PublicRunEventKind::RunInterrupted { .. }
                | PublicRunEventKind::RouteRecoveryRequired { .. }
                | PublicRunEventKind::RunCancelled
        );
        let mut latest_sequences = self
            .latest_sequences
            .lock()
            .expect("http live sequence watermark lock should not be poisoned");
        let stream_closed = close_stream_after_event || (terminal && !keep_stream_open);
        if stream_closed {
            latest_sequences.remove(&sequence_key);
        } else {
            latest_sequences
                .entry(sequence_key)
                .and_modify(|sequence| *sequence = (*sequence).max(event.run_event.sequence))
                .or_insert(event.run_event.sequence);
        }
        drop(latest_sequences);
        let _ = self
            .sender
            .send(HttpLiveBusMessage::Event(Box::new(event.clone())));
        if stream_closed {
            let _ = self.sender.send(HttpLiveBusMessage::StreamClosed {
                session_id: event.run_event.session_id.clone(),
                run_id: event.run_event.run_id.clone(),
            });
        }
        Ok(event)
    }

    /// Allocates the next public sequence under the bus publication lock, then publishes the
    /// event. Production adapters use this for every event source so foreground output and
    /// out-of-band terminal lifecycle callbacks cannot race sequence allocation.
    pub fn publish_next_run_event(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_next_run_event_with_policy(event, None, false, false)
    }

    /// Allocates and publishes a foreground terminal while retaining its owned terminal stream.
    pub fn publish_next_run_event_with_stream_continuation(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_next_run_event_with_policy(event, None, true, false)
    }

    /// Allocates and publishes the final lifecycle while atomically closing the retained stream.
    pub fn publish_next_run_event_and_close_stream(
        &self,
        event: PublicRunEvent,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        self.publish_next_run_event_with_policy(event, None, false, true)
    }

    /// Allocates an approval sequence and lets the caller bind that sequence into the durable
    /// protocol projection before publication. The production adapter pre-registers routing
    /// state before entering this publication lock, which keeps registry and event-bus lock order
    /// acyclic while ensuring subscribers never observe an unroutable approval.
    pub fn publish_next_run_event_with_approval<F>(
        &self,
        mut event: PublicRunEvent,
        register: F,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError>
    where
        F: FnOnce(u64) -> Result<HttpPendingApproval, HttpEventPublishError>,
    {
        let _publication =
            self.publication_lock
                .lock()
                .map_err(|_| HttpEventPublishError::Journal {
                    message: "http event publication sequencer is unavailable".to_owned(),
                })?;
        let latest = self
            .latest_run_sequence(&event.session_id, &event.run_id)
            .map_err(|error| HttpEventPublishError::Journal {
                message: error.to_string(),
            })?
            .unwrap_or(0);
        event.sequence = latest.saturating_add(1);
        let approval_request = register(event.sequence)?;
        self.publish_run_event_with_policy_locked(event, Some(approval_request), false, false)
    }

    fn publish_next_run_event_with_policy(
        &self,
        mut event: PublicRunEvent,
        approval_request: Option<HttpPendingApproval>,
        keep_stream_open: bool,
        close_stream_after_event: bool,
    ) -> Result<HttpProtocolEvent, HttpEventPublishError> {
        let _publication =
            self.publication_lock
                .lock()
                .map_err(|_| HttpEventPublishError::Journal {
                    message: "http event publication sequencer is unavailable".to_owned(),
                })?;
        let latest = self
            .latest_run_sequence(&event.session_id, &event.run_id)
            .map_err(|error| HttpEventPublishError::Journal {
                message: error.to_string(),
            })?
            .unwrap_or(0);
        event.sequence = latest.saturating_add(1);
        self.publish_run_event_with_policy_locked(
            event,
            approval_request,
            keep_stream_open,
            close_stream_after_event,
        )
    }

    /// Closes a stream retained past the foreground terminal after every terminal task settles.
    pub fn close_run_stream(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<(), HttpEventPublishError> {
        let _publication =
            self.publication_lock
                .lock()
                .map_err(|_| HttpEventPublishError::Journal {
                    message: "http event publication sequencer is unavailable".to_owned(),
                })?;
        if let Some(journal) = &self.durable_journal {
            journal.close_stream(session_id, run_id).map_err(|error| {
                HttpEventPublishError::Journal {
                    message: error.to_string(),
                }
            })?;
        }
        self.latest_sequences
            .lock()
            .expect("http live sequence watermark lock should not be poisoned")
            .remove(&HttpRunSequenceKey {
                session_id: session_id.to_owned(),
                run_id: run_id.to_owned(),
            });
        let _ = self.sender.send(HttpLiveBusMessage::StreamClosed {
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
        });
        Ok(())
    }

    /// Reads the latest published per-run sequence without exposing replay payloads.
    ///
    /// # Errors
    ///
    /// Returns an error when a configured durable journal cannot be read safely.
    pub fn latest_run_sequence(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<Option<u64>, HttpProtocolReplayError> {
        let live = self
            .latest_sequences
            .lock()
            .map_err(|_| HttpProtocolReplayError::JournalUnavailable)?
            .get(&HttpRunSequenceKey {
                session_id: session_id.to_owned(),
                run_id: run_id.to_owned(),
            })
            .copied();
        let durable = self
            .durable_journal
            .as_ref()
            .map(|journal| journal.latest_run_sequence(session_id, run_id))
            .transpose()
            .map_err(|_| HttpProtocolReplayError::JournalUnavailable)?
            .flatten();
        let buffered = self
            .durable_journal
            .is_none()
            .then(|| self.buffer.latest_run_sequence(session_id, run_id))
            .flatten();
        Ok([live, durable, buffered].into_iter().flatten().max())
    }

    /// Returns whether the exact run stream remains open for later owner-published events.
    pub fn run_stream_accepts_events(
        &self,
        session_id: &str,
        run_id: &str,
    ) -> Result<bool, HttpProtocolReplayError> {
        if let Some(journal) = &self.durable_journal {
            return journal
                .stream_accepts_events(session_id, run_id)
                .map(|accepts| accepts.unwrap_or(false))
                .map_err(|_| HttpProtocolReplayError::JournalUnavailable);
        }
        self.latest_sequences
            .lock()
            .map_err(|_| HttpProtocolReplayError::JournalUnavailable)
            .map(|sequences| {
                sequences.contains_key(&HttpRunSequenceKey {
                    session_id: session_id.to_owned(),
                    run_id: run_id.to_owned(),
                })
            })
    }

    #[cfg(test)]
    pub(crate) fn synthetic_buffer_len(&self) -> usize {
        self.buffer
            .events
            .lock()
            .expect("http protocol event buffer lock should not be poisoned")
            .len()
    }

    #[cfg(test)]
    pub(crate) fn active_sequence_watermark_len(&self) -> usize {
        self.latest_sequences
            .lock()
            .expect("http live sequence watermark lock should not be poisoned")
            .len()
    }

    /// Replays durable events for one run after an optional cursor.
    ///
    /// # Errors
    ///
    /// Returns an error when the cursor is invalid, wrong-scope, or ahead of the buffer.
    pub fn replay_run_after(
        &self,
        session_id: &str,
        run_id: &str,
        last_event_id: Option<&str>,
    ) -> Result<Vec<HttpProtocolEvent>, HttpProtocolReplayError> {
        match &self.durable_journal {
            Some(journal) => journal.replay_run_after(session_id, run_id, last_event_id),
            None => self
                .buffer
                .replay_run_after(session_id, run_id, last_event_id),
        }
    }
}

/// Subscriber for bounded local live events.
pub struct HttpLiveEventSubscriber {
    receiver: broadcast::Receiver<HttpLiveBusMessage>,
}

pub(crate) enum HttpRunStreamReceive {
    Event(Box<HttpProtocolEvent>),
    StreamClosed { session_id: String, run_id: String },
}

impl HttpLiveEventSubscriber {
    /// Receives one live protocol event.
    ///
    /// # Errors
    ///
    /// Returns `Lagged` when bounded live capacity dropped events, or `Closed` when the bus closes.
    pub async fn recv(&mut self) -> Result<HttpProtocolEvent, HttpLiveEventRecvError> {
        loop {
            match self.receiver.recv().await.map_err(map_live_recv_error)? {
                HttpLiveBusMessage::Event(event) => return Ok(*event),
                HttpLiveBusMessage::StreamClosed { .. } => {}
            }
        }
    }

    pub(crate) async fn recv_run_stream(
        &mut self,
    ) -> Result<HttpRunStreamReceive, HttpLiveEventRecvError> {
        match self.receiver.recv().await.map_err(map_live_recv_error)? {
            HttpLiveBusMessage::Event(event) => Ok(HttpRunStreamReceive::Event(event)),
            HttpLiveBusMessage::StreamClosed { session_id, run_id } => {
                Ok(HttpRunStreamReceive::StreamClosed { session_id, run_id })
            }
        }
    }
}

fn map_live_recv_error(error: broadcast::error::RecvError) -> HttpLiveEventRecvError {
    match error {
        broadcast::error::RecvError::Closed => HttpLiveEventRecvError::Closed,
        broadcast::error::RecvError::Lagged(dropped) => HttpLiveEventRecvError::Lagged { dropped },
    }
}

/// Serializes one public run event into an SSE frame.
///
/// # Errors
///
/// Returns an error when the public event cannot be serialized.
pub fn public_run_event_to_sse(event: &PublicRunEvent) -> Result<HttpSseEvent, HttpSseError> {
    let protocol_event =
        HttpProtocolEvent::from_run_event(event.clone()).map_err(|error| HttpSseError::Cursor {
            message: error.to_string(),
        })?;
    let data = serde_json::to_string(&protocol_event).map_err(|error| HttpSseError::Serialize {
        message: error.to_string(),
    })?;
    HttpSseEvent::with_id(protocol_event.replay_id, HTTP_RUN_EVENT_SSE_NAME, data)
}

/// Sequence generator for public run events emitted by the HTTP adapter.
#[derive(Default)]
pub struct HttpRunEventSequencer {
    state: Mutex<BTreeMap<HttpRunSequenceKey, u64>>,
}

impl HttpRunEventSequencer {
    /// Creates an empty sequencer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates the next public event for a session/run pair.
    pub fn next_public_event(
        &self,
        session_id: &str,
        run_id: &str,
        event: PublicRunEventKind,
    ) -> PublicRunEvent {
        let sequence = self.next_sequence(session_id, run_id);
        PublicRunEvent::new(session_id, run_id, sequence, event)
    }

    /// Creates the next SSE frame for a session/run pair.
    ///
    /// # Errors
    ///
    /// Returns an error when the public event cannot be serialized.
    pub fn next_sse_event(
        &self,
        session_id: &str,
        run_id: &str,
        event: PublicRunEventKind,
    ) -> Result<HttpSseEvent, HttpSseError> {
        let event = self.next_public_event(session_id, run_id, event);
        public_run_event_to_sse(&event)
    }

    fn next_sequence(&self, session_id: &str, run_id: &str) -> u64 {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        let key = HttpRunSequenceKey {
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
        };
        let sequence = state.entry(key).or_insert(0);
        *sequence += 1;
        *sequence
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HttpRunSequenceKey {
    session_id: String,
    run_id: String,
}

fn append_sse_field(buffer: &mut String, field: &str, value: &str) {
    for line in value.split('\n') {
        buffer.push_str(field);
        buffer.push_str(": ");
        buffer.push_str(line);
        buffer.push('\n');
    }
}

fn protocol_event_class(event: &PublicRunEventKind) -> HttpProtocolEventClass {
    match event {
        PublicRunEventKind::TextDelta { .. }
        | PublicRunEventKind::ReasoningDelta { .. }
        | PublicRunEventKind::ToolCallArgsDelta { .. }
        | PublicRunEventKind::ToolProgress { .. } => HttpProtocolEventClass::Transient,
        PublicRunEventKind::RouteTransition { .. }
        | PublicRunEventKind::RunStarted { .. }
        | PublicRunEventKind::TaskRunStarted { .. }
        | PublicRunEventKind::RunFinished { .. }
        | PublicRunEventKind::RunAwaitingUserInput { .. }
        | PublicRunEventKind::TaskRunFinished { .. }
        | PublicRunEventKind::TaskRoutingChanged { .. }
        | PublicRunEventKind::ConversationRouteChanged { .. }
        | PublicRunEventKind::PlanReviewChanged { .. }
        | PublicRunEventKind::UserInputChanged { .. }
        | PublicRunEventKind::TaskPhaseChanged { .. }
        | PublicRunEventKind::TaskExecutionAdmitted { .. }
        | PublicRunEventKind::TaskPlanUpdated { .. }
        | PublicRunEventKind::TaskChecklistUpdated { .. }
        | PublicRunEventKind::TaskBatchChanged { .. }
        | PublicRunEventKind::TaskStepChanged { .. }
        | PublicRunEventKind::IntegrationLaneChanged { .. }
        | PublicRunEventKind::ProviderTurnRecoveryChanged { .. }
        | PublicRunEventKind::ProviderTurnPartialOutputDiscarded { .. }
        | PublicRunEventKind::RunFailed { .. }
        | PublicRunEventKind::RunBlocked { .. }
        | PublicRunEventKind::RunPaused { .. }
        | PublicRunEventKind::RunInterrupted { .. }
        | PublicRunEventKind::RouteRecoveryRequired { .. }
        | PublicRunEventKind::RunCancelled
        | PublicRunEventKind::ToolCallStarted { .. }
        | PublicRunEventKind::ToolCallCompleted { .. }
        | PublicRunEventKind::ApprovalRequested { .. }
        | PublicRunEventKind::ApprovalResolved { .. }
        | PublicRunEventKind::ToolResult { .. }
        | PublicRunEventKind::Usage { .. }
        | PublicRunEventKind::ContinuationState { .. }
        | PublicRunEventKind::TerminalLifecycle { .. }
        | PublicRunEventKind::Control { .. }
        | PublicRunEventKind::AssistantMessage { .. }
        | PublicRunEventKind::Notice { .. } => HttpProtocolEventClass::Durable,
    }
}

fn validate_cursor_component(
    component: &'static str,
    value: &str,
) -> Result<(), HttpProtocolCursorError> {
    if value.trim().is_empty()
        || value.contains(':')
        || value.contains('\r')
        || value.contains('\n')
    {
        return Err(HttpProtocolCursorError::InvalidComponent {
            component,
            value: value.to_owned(),
        });
    }
    Ok(())
}
