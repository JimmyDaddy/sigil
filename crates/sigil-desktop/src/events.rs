use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::DesktopPendingApproval;

/// Current HTTP protocol-event envelope accepted by the desktop client.
pub const DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION: u32 = 2;
/// Current public run-event envelope accepted by the desktop client.
pub const DESKTOP_PUBLIC_RUN_EVENT_SCHEMA_VERSION: u32 = 1;

const MAX_TIMELINE_TEXT_BYTES: usize = 128 * 1024;
const MAX_MACHINE_LABEL_BYTES: usize = 512;

/// Replay classification attached to one HTTP protocol event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopProtocolEventClass {
    Durable,
    Transient,
}

/// Typed HTTP protocol envelope consumed from the server-owned SSE stream.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopProtocolEvent {
    pub schema_version: u32,
    pub event_class: DesktopProtocolEventClass,
    #[serde(default)]
    pub replay_id: Option<String>,
    #[serde(default)]
    pub approval_request: Option<DesktopPendingApproval>,
    #[serde(default)]
    pub provisional_id: Option<String>,
    pub run_event: DesktopPublicRunEvent,
}

/// Typed public run envelope consumed by the native client.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopPublicRunEvent {
    pub schema_version: u32,
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event: DesktopPublicRunEventKind,
}

/// Provider-neutral public task phase mirrored from the versioned HTTP contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopPublicTaskPhase {
    Routing,
    Planning,
    Execution,
    Integration,
    Synthesis,
    Terminal,
}

/// Bounded task-plan step safe to forward to the renderer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicTaskPlanStep {
    pub step_id: String,
    pub title: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub mode: String,
    pub isolation: String,
}

/// Renderer-facing task-plan step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopTimelineTaskPlanStep {
    pub step_id: String,
    pub title: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub mode: String,
    pub isolation: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicToolCall {
    pub id: String,
    pub name: String,
    pub args_json: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicToolProgress {
    pub call_id: String,
    pub tool_name: String,
    pub status: String,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicToolResult {
    pub call_id: String,
    pub tool_name: String,
    pub content: String,
    #[serde(deserialize_with = "deserialize_tool_result_status")]
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicAssistantMessage {
    pub id: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub assistant_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicToolPreview {
    pub title: String,
    pub summary: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct DesktopPublicControlEvent {
    pub kind: String,
}

/// Typed public event payload.
///
/// Every event currently emitted by the HTTP adapter has an explicit variant. Unknown future
/// non-breaking event types remain attachable and project to `other` without exposing their raw
/// payload to the renderer.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DesktopPublicRunEventKind {
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
    TaskRoutingChanged {
        handoff_id: String,
        status: String,
        #[serde(default)]
        task_id: Option<String>,
    },
    TaskPhaseChanged {
        #[serde(default)]
        task_id: Option<String>,
        phase: DesktopPublicTaskPhase,
        status: String,
    },
    TaskPlanUpdated {
        task_id: String,
        plan_version: u32,
        status: String,
        steps: Vec<DesktopPublicTaskPlanStep>,
    },
    TaskBatchChanged {
        task_id: String,
        plan_version: u32,
        batch_id: String,
        active: u32,
        completed: u32,
        failed: u32,
    },
    TaskStepChanged {
        task_id: String,
        plan_version: u32,
        step_id: String,
        #[serde(default)]
        attempt_id: Option<String>,
        status: String,
    },
    IntegrationLaneChanged {
        task_id: String,
        plan_version: u32,
        plan_id: String,
        lane_id: String,
        status: String,
        conflicts: Vec<String>,
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
        call: DesktopPublicToolCall,
    },
    ToolCallArgsDelta {
        id: String,
        delta: String,
    },
    ToolCallCompleted {
        call: DesktopPublicToolCall,
    },
    ApprovalRequested {
        call: DesktopPublicToolCall,
        #[serde(default)]
        operation: Option<String>,
        #[serde(default)]
        risk: Option<String>,
        #[serde(default)]
        snapshot_required: bool,
        #[serde(default)]
        preview: Option<DesktopPublicToolPreview>,
    },
    ApprovalResolved {
        call_id: String,
        approved: bool,
        #[serde(default)]
        reason: Option<String>,
    },
    ToolResult {
        result: DesktopPublicToolResult,
    },
    ToolProgress {
        progress: DesktopPublicToolProgress,
    },
    Usage {},
    ContinuationState {},
    Control {
        control: DesktopPublicControlEvent,
    },
    AssistantMessage {
        message: DesktopPublicAssistantMessage,
    },
    Notice {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

/// Renderer-facing event categories. These are presentation facts, not a second run state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTimelineEventKind {
    RunStarted,
    TaskRunStarted,
    TaskRunFinished,
    TaskRoutingChanged,
    TaskPhaseChanged,
    TaskPlanUpdated,
    TaskBatchChanged,
    TaskStepChanged,
    IntegrationLaneChanged,
    AssistantDelta,
    ReasoningDelta,
    AssistantMessage,
    ToolStarted,
    ToolCompleted,
    ToolProgress,
    ToolResult,
    ApprovalRequested,
    ApprovalResolved,
    Notice,
    Usage,
    Control,
    RunFinished,
    RunFailed,
    RunCancelled,
    Other,
}

/// Typed task projection safe to send to the local renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopTimelineTask {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<DesktopPublicTaskPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lane_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<DesktopTimelineTaskPlanStep>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
}

/// Narrow approval summary safe to send to the local renderer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopTimelineApproval {
    pub call_id: String,
    pub tool_name: String,
    pub approval_request_id: String,
    pub tool_call_hash: String,
    pub policy_version: String,
    pub expires_at_ms: u64,
    pub session_grant_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    pub snapshot_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_body: Option<String>,
}

/// Bounded, credential-free timeline event emitted by the native desktop backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopTimelineEvent {
    pub workspace_id: String,
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
    /// Exact decimal representation retained for JavaScript consumers.
    pub run_sequence: String,
    pub replayable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provisional_id: Option<String>,
    pub kind: DesktopTimelineEventKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval: Option<DesktopTimelineApproval>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task: Option<DesktopTimelineTask>,
}

impl DesktopProtocolEvent {
    /// Validates one event against the requested stream and narrows it for renderer delivery.
    pub fn into_timeline(
        self,
        workspace_id: &str,
        expected_session_id: &str,
        expected_run_id: &str,
        renderer_session_id: &str,
    ) -> Result<DesktopTimelineEvent, DesktopProtocolEventError> {
        self.validate(expected_session_id, expected_run_id)?;
        let event = &self.run_event.event;
        let tool_call = match event {
            DesktopPublicRunEventKind::ToolCallStarted { call }
            | DesktopPublicRunEventKind::ToolCallCompleted { call }
            | DesktopPublicRunEventKind::ApprovalRequested { call, .. } => Some(call),
            _ => None,
        };
        let tool_name = tool_call
            .map(|call| bounded_text(&call.name))
            .or_else(|| match event {
                DesktopPublicRunEventKind::ToolResult { result } => {
                    Some(bounded_text(&result.tool_name))
                }
                DesktopPublicRunEventKind::ToolProgress { progress } => {
                    Some(bounded_text(&progress.tool_name))
                }
                _ => None,
            });
        let tool_input = tool_call.and_then(project_tool_input);
        let assistant_kind = match event {
            DesktopPublicRunEventKind::AssistantMessage { message } => {
                message.assistant_kind.as_deref().map(bounded_text)
            }
            _ => None,
        };
        let (kind, text, item_id, status) = match event {
            DesktopPublicRunEventKind::RunStarted { prompt } => (
                DesktopTimelineEventKind::RunStarted,
                Some(bounded_text(prompt)),
                None,
                None,
            ),
            DesktopPublicRunEventKind::TaskRunStarted { .. } => (
                DesktopTimelineEventKind::TaskRunStarted,
                None,
                None,
                Some("running".to_owned()),
            ),
            DesktopPublicRunEventKind::TaskRunFinished { status, .. } => (
                DesktopTimelineEventKind::TaskRunFinished,
                None,
                None,
                Some(bounded_text(status)),
            ),
            DesktopPublicRunEventKind::TaskRoutingChanged {
                handoff_id, status, ..
            } => (
                DesktopTimelineEventKind::TaskRoutingChanged,
                None,
                Some(bounded_text(handoff_id)),
                Some(bounded_text(status)),
            ),
            DesktopPublicRunEventKind::TaskPhaseChanged { status, .. } => (
                DesktopTimelineEventKind::TaskPhaseChanged,
                None,
                None,
                Some(bounded_text(status)),
            ),
            DesktopPublicRunEventKind::TaskPlanUpdated { status, .. } => (
                DesktopTimelineEventKind::TaskPlanUpdated,
                None,
                None,
                Some(bounded_text(status)),
            ),
            DesktopPublicRunEventKind::TaskBatchChanged {
                batch_id, failed, ..
            } => (
                DesktopTimelineEventKind::TaskBatchChanged,
                None,
                Some(bounded_text(batch_id)),
                Some(if *failed == 0 { "running" } else { "failed" }.to_owned()),
            ),
            DesktopPublicRunEventKind::TaskStepChanged {
                step_id, status, ..
            } => (
                DesktopTimelineEventKind::TaskStepChanged,
                None,
                Some(bounded_text(step_id)),
                Some(bounded_text(status)),
            ),
            DesktopPublicRunEventKind::IntegrationLaneChanged {
                lane_id, status, ..
            } => (
                DesktopTimelineEventKind::IntegrationLaneChanged,
                None,
                Some(bounded_text(lane_id)),
                Some(bounded_text(status)),
            ),
            DesktopPublicRunEventKind::TextDelta { text } => (
                DesktopTimelineEventKind::AssistantDelta,
                Some(bounded_text(text)),
                None,
                None,
            ),
            DesktopPublicRunEventKind::ReasoningDelta { text } => (
                DesktopTimelineEventKind::ReasoningDelta,
                Some(bounded_text(text)),
                None,
                None,
            ),
            DesktopPublicRunEventKind::AssistantMessage { message } => (
                DesktopTimelineEventKind::AssistantMessage,
                message.content.as_deref().map(bounded_text),
                Some(bounded_text(&message.id)),
                None,
            ),
            DesktopPublicRunEventKind::ToolCallStarted { call } => (
                DesktopTimelineEventKind::ToolStarted,
                None,
                Some(bounded_text(&call.id)),
                Some("running".to_owned()),
            ),
            DesktopPublicRunEventKind::ToolCallCompleted { call } => (
                DesktopTimelineEventKind::ToolCompleted,
                None,
                Some(bounded_text(&call.id)),
                Some("ready".to_owned()),
            ),
            DesktopPublicRunEventKind::ToolProgress { progress } => (
                DesktopTimelineEventKind::ToolProgress,
                progress.message.as_deref().map(bounded_text),
                Some(bounded_text(&progress.call_id)),
                Some(bounded_text(&progress.status)),
            ),
            DesktopPublicRunEventKind::ToolResult { result } => (
                DesktopTimelineEventKind::ToolResult,
                Some(bounded_text(&result.content)),
                Some(bounded_text(&result.call_id)),
                Some(bounded_text(&result.status)),
            ),
            DesktopPublicRunEventKind::ApprovalRequested { call, .. } => (
                DesktopTimelineEventKind::ApprovalRequested,
                None,
                Some(bounded_text(&call.id)),
                Some("waiting".to_owned()),
            ),
            DesktopPublicRunEventKind::ApprovalResolved {
                call_id,
                approved,
                reason,
            } => (
                DesktopTimelineEventKind::ApprovalResolved,
                reason.as_deref().map(bounded_text),
                Some(bounded_text(call_id)),
                Some(if *approved { "approved" } else { "denied" }.to_owned()),
            ),
            DesktopPublicRunEventKind::Notice { message } => (
                DesktopTimelineEventKind::Notice,
                Some(bounded_text(message)),
                None,
                None,
            ),
            DesktopPublicRunEventKind::Usage {} => {
                (DesktopTimelineEventKind::Usage, None, None, None)
            }
            DesktopPublicRunEventKind::Control { control } => (
                DesktopTimelineEventKind::Control,
                None,
                Some(bounded_text(&control.kind)),
                None,
            ),
            DesktopPublicRunEventKind::RunFinished { final_text } => (
                DesktopTimelineEventKind::RunFinished,
                Some(bounded_text(final_text)),
                None,
                Some("finished".to_owned()),
            ),
            DesktopPublicRunEventKind::RunFailed { error } => (
                DesktopTimelineEventKind::RunFailed,
                Some(bounded_text(error)),
                None,
                Some("failed".to_owned()),
            ),
            DesktopPublicRunEventKind::RunCancelled => (
                DesktopTimelineEventKind::RunCancelled,
                None,
                None,
                Some("cancelled".to_owned()),
            ),
            DesktopPublicRunEventKind::ToolCallArgsDelta { .. }
            | DesktopPublicRunEventKind::ContinuationState {}
            | DesktopPublicRunEventKind::Unknown => {
                (DesktopTimelineEventKind::Other, None, None, None)
            }
        };
        let task = project_task_event(event)?;
        let approval = if kind == DesktopTimelineEventKind::ApprovalRequested {
            Some(self.approval_view(tool_name.as_deref())?)
        } else {
            None
        };
        Ok(DesktopTimelineEvent {
            workspace_id: bounded_machine_label(workspace_id)?,
            session_id: bounded_machine_label(renderer_session_id)?,
            run_id: self.run_event.run_id,
            sequence: self.run_event.sequence,
            run_sequence: self.run_event.sequence.to_string(),
            replayable: self.event_class == DesktopProtocolEventClass::Durable,
            replay_id: self.replay_id,
            provisional_id: self.provisional_id,
            kind,
            text,
            item_id,
            tool_name,
            status,
            assistant_kind,
            tool_input,
            approval,
            task,
        })
    }

    pub(crate) fn validate(
        &self,
        expected_session_id: &str,
        expected_run_id: &str,
    ) -> Result<(), DesktopProtocolEventError> {
        if self.schema_version != DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION
            || self.run_event.schema_version != DESKTOP_PUBLIC_RUN_EVENT_SCHEMA_VERSION
        {
            return Err(DesktopProtocolEventError::UnsupportedSchema);
        }
        if self.run_event.session_id != expected_session_id
            || self.run_event.run_id != expected_run_id
            || self.run_event.sequence == 0
        {
            return Err(DesktopProtocolEventError::WrongStream);
        }
        bounded_machine_label(&self.run_event.session_id)?;
        bounded_machine_label(&self.run_event.run_id)?;
        match self.event_class {
            DesktopProtocolEventClass::Durable => {
                let replay_id = self
                    .replay_id
                    .as_deref()
                    .ok_or(DesktopProtocolEventError::InvalidReplayCursor)?;
                bounded_cursor(replay_id)?;
            }
            DesktopProtocolEventClass::Transient if self.replay_id.is_some() => {
                return Err(DesktopProtocolEventError::InvalidReplayCursor);
            }
            DesktopProtocolEventClass::Transient => {}
        }
        if let Some(provisional_id) = self.provisional_id.as_deref()
            && !valid_provisional_id(provisional_id)
        {
            return Err(DesktopProtocolEventError::InvalidProvisionalIdentity);
        }
        Ok(())
    }

    fn approval_view(
        &self,
        projected_tool_name: Option<&str>,
    ) -> Result<DesktopTimelineApproval, DesktopProtocolEventError> {
        let guard = self
            .approval_request
            .as_ref()
            .ok_or(DesktopProtocolEventError::InvalidApproval)?;
        if projected_tool_name != Some(guard.tool_name.as_str()) {
            return Err(DesktopProtocolEventError::InvalidApproval);
        }
        let DesktopPublicRunEventKind::ApprovalRequested {
            call,
            operation,
            risk,
            snapshot_required,
            preview,
        } = &self.run_event.event
        else {
            return Err(DesktopProtocolEventError::InvalidApproval);
        };
        Ok(DesktopTimelineApproval {
            call_id: bounded_machine_label(&guard.call_id)?,
            tool_name: bounded_machine_label(&guard.tool_name)?,
            approval_request_id: bounded_machine_label(&guard.approval_request_id)?,
            tool_call_hash: bounded_machine_label(&guard.tool_call_hash)?,
            policy_version: bounded_machine_label(&guard.policy_version)?,
            expires_at_ms: guard.expires_at_ms,
            session_grant_available: guard.session_grant_available,
            tool_input: project_tool_input(call),
            operation: operation.as_deref().map(bounded_text),
            risk: risk.as_deref().map(bounded_text),
            snapshot_required: *snapshot_required,
            preview_title: preview.as_ref().map(|preview| bounded_text(&preview.title)),
            preview_summary: preview
                .as_ref()
                .map(|preview| bounded_text(&preview.summary)),
            preview_body: preview.as_ref().map(|preview| bounded_text(&preview.body)),
        })
    }
}

fn project_tool_input(call: &DesktopPublicToolCall) -> Option<String> {
    let args = serde_json::from_str::<Value>(&call.args_json).ok()?;
    let value = match call.name.as_str() {
        "bash" | "shell" | "terminal_start" => {
            let command = args.get("command")?.as_str()?;
            if command_contains_credential_shape(command) {
                "[credential-shaped command arguments redacted]".to_owned()
            } else {
                command.to_owned()
            }
        }
        "read_file" | "write_file" | "delete_file" | "edit_file" => {
            let path = args.get("path")?.as_str()?;
            if !renderer_safe_relative_path(path) {
                return None;
            }
            format!("path={path}")
        }
        "grep" | "search" => project_named_string_fields(&args, &["pattern", "path"])?,
        "glob" => project_named_string_fields(&args, &["pattern", "path"])?,
        "ls" | "list_files" => project_named_string_fields(&args, &["path"])?,
        "websearch" | "web_search" => project_named_string_fields(&args, &["query"])?,
        "terminal_input" => project_named_string_fields(&args, &["task_id"])?,
        _ => return None,
    };
    Some(bounded_text(&value))
}

fn project_task_event(
    event: &DesktopPublicRunEventKind,
) -> Result<Option<DesktopTimelineTask>, DesktopProtocolEventError> {
    let task = match event {
        DesktopPublicRunEventKind::TaskRunStarted { task_id, objective } => DesktopTimelineTask {
            task_id: Some(bounded_machine_label(task_id)?),
            objective: Some(bounded_text(objective)),
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::TaskRunFinished { task_id, .. } => DesktopTimelineTask {
            task_id: Some(bounded_machine_label(task_id)?),
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::TaskRoutingChanged {
            handoff_id,
            task_id,
            ..
        } => DesktopTimelineTask {
            task_id: bounded_optional_machine_label(task_id.as_deref())?,
            handoff_id: Some(bounded_machine_label(handoff_id)?),
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::TaskPhaseChanged { task_id, phase, .. } => DesktopTimelineTask {
            task_id: bounded_optional_machine_label(task_id.as_deref())?,
            phase: Some(*phase),
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::TaskPlanUpdated {
            task_id,
            plan_version,
            steps,
            ..
        } => DesktopTimelineTask {
            task_id: Some(bounded_machine_label(task_id)?),
            plan_version: Some(*plan_version),
            steps: steps
                .iter()
                .map(project_task_plan_step)
                .collect::<Result<Vec<_>, _>>()?,
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::TaskBatchChanged {
            task_id,
            plan_version,
            batch_id,
            active,
            completed,
            failed,
        } => DesktopTimelineTask {
            task_id: Some(bounded_machine_label(task_id)?),
            plan_version: Some(*plan_version),
            batch_id: Some(bounded_machine_label(batch_id)?),
            active: Some(*active),
            completed: Some(*completed),
            failed: Some(*failed),
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::TaskStepChanged {
            task_id,
            plan_version,
            step_id,
            attempt_id,
            ..
        } => DesktopTimelineTask {
            task_id: Some(bounded_machine_label(task_id)?),
            plan_version: Some(*plan_version),
            step_id: Some(bounded_machine_label(step_id)?),
            attempt_id: bounded_optional_machine_label(attempt_id.as_deref())?,
            ..DesktopTimelineTask::default()
        },
        DesktopPublicRunEventKind::IntegrationLaneChanged {
            task_id,
            plan_version,
            plan_id,
            lane_id,
            conflicts,
            ..
        } => DesktopTimelineTask {
            task_id: Some(bounded_machine_label(task_id)?),
            plan_version: Some(*plan_version),
            plan_id: Some(bounded_machine_label(plan_id)?),
            lane_id: Some(bounded_machine_label(lane_id)?),
            conflicts: conflicts.iter().map(|value| bounded_text(value)).collect(),
            ..DesktopTimelineTask::default()
        },
        _ => return Ok(None),
    };
    Ok(Some(task))
}

fn project_task_plan_step(
    step: &DesktopPublicTaskPlanStep,
) -> Result<DesktopTimelineTaskPlanStep, DesktopProtocolEventError> {
    Ok(DesktopTimelineTaskPlanStep {
        step_id: bounded_machine_label(&step.step_id)?,
        title: bounded_text(&step.title),
        role: bounded_machine_label(&step.role)?,
        depends_on: step
            .depends_on
            .iter()
            .map(|dependency| bounded_machine_label(dependency))
            .collect::<Result<Vec<_>, _>>()?,
        mode: bounded_machine_label(&step.mode)?,
        isolation: bounded_machine_label(&step.isolation)?,
    })
}

fn bounded_optional_machine_label(
    value: Option<&str>,
) -> Result<Option<String>, DesktopProtocolEventError> {
    value.map(bounded_machine_label).transpose()
}

fn command_contains_credential_shape(command: &str) -> bool {
    let normalized = command.to_ascii_lowercase();
    [
        "api_key=",
        "apikey=",
        "password=",
        "secret=",
        "token=",
        "authorization:",
        "bearer ",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn renderer_safe_relative_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with(['/', '\\', '~'])
        && !path.split(['/', '\\']).any(|segment| segment == "..")
        && path.as_bytes().get(1).is_none_or(|byte| *byte != b':')
}

fn project_named_string_fields(args: &Value, names: &[&str]) -> Option<String> {
    let fields = names
        .iter()
        .filter_map(|name| {
            args.get(*name)
                .and_then(Value::as_str)
                .map(|value| format!("{name}={value}"))
        })
        .collect::<Vec<_>>();
    (!fields.is_empty()).then(|| fields.join("\n"))
}

fn deserialize_tool_result_status<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let status = Value::deserialize(deserializer)?;
    let label = status
        .as_str()
        .or_else(|| {
            status
                .as_object()
                .and_then(|object| object.keys().next())
                .map(String::as_str)
        })
        .ok_or_else(|| serde::de::Error::custom("tool result status must have a discriminant"))?;
    Ok(label.to_owned())
}

fn bounded_machine_label(value: &str) -> Result<String, DesktopProtocolEventError> {
    if value.is_empty()
        || value.len() > MAX_MACHINE_LABEL_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(DesktopProtocolEventError::InvalidMachineLabel);
    }
    Ok(value.to_owned())
}

fn bounded_cursor(value: &str) -> Result<(), DesktopProtocolEventError> {
    if value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control) {
        return Err(DesktopProtocolEventError::InvalidReplayCursor);
    }
    Ok(())
}

fn valid_provisional_id(value: &str) -> bool {
    value.strip_prefix("live-v1:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    })
}

fn bounded_text(value: &str) -> String {
    if value.len() <= MAX_TIMELINE_TEXT_BYTES {
        return value.to_owned();
    }
    let mut end = MAX_TIMELINE_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[… desktop preview truncated]", &value[..end])
}

/// Safe protocol projection errors that never include event payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DesktopProtocolEventError {
    #[error("desktop event schema is unsupported")]
    UnsupportedSchema,
    #[error("desktop event belongs to a different stream")]
    WrongStream,
    #[error("desktop event replay cursor is invalid")]
    InvalidReplayCursor,
    #[error("desktop event live provisional identity is invalid")]
    InvalidProvisionalIdentity,
    #[error("desktop event machine label is invalid")]
    InvalidMachineLabel,
    #[error("desktop event payload is invalid")]
    InvalidPayload,
    #[error("desktop approval event is invalid")]
    InvalidApproval,
}

#[cfg(test)]
#[path = "tests/events_tests.rs"]
mod tests;
