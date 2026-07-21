use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::DesktopPendingApproval;

/// Current HTTP protocol-event envelope accepted by the desktop client.
pub const DESKTOP_PROTOCOL_EVENT_SCHEMA_VERSION: u32 = 1;
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
    pub run_event: DesktopPublicRunEvent,
}

/// Typed public run envelope. The payload remains provider-neutral JSON and is narrowed before IPC.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct DesktopPublicRunEvent {
    pub schema_version: u32,
    pub session_id: String,
    pub run_id: String,
    pub sequence: u64,
    pub event: Value,
}

/// Renderer-facing event categories. These are presentation facts, not a second run state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesktopTimelineEventKind {
    RunStarted,
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
    pub replayable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replay_id: Option<String>,
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
        let event_type = self
            .run_event
            .event
            .get("type")
            .and_then(Value::as_str)
            .ok_or(DesktopProtocolEventError::InvalidPayload)?;
        let field = |name: &str| {
            self.run_event
                .event
                .get(name)
                .and_then(Value::as_str)
                .map(bounded_text)
        };
        let nested_string = |object: &str, name: &str| {
            self.run_event
                .event
                .get(object)
                .and_then(|value| value.get(name))
                .and_then(Value::as_str)
                .map(bounded_text)
        };
        let call_id = nested_string("call", "id");
        let tool_name = nested_string("call", "name")
            .or_else(|| nested_string("result", "tool_name"))
            .or_else(|| nested_string("progress", "tool_name"));
        let assistant_kind = if event_type == "assistant_message" {
            nested_string("message", "assistant_kind")
        } else {
            None
        };
        let tool_input = project_tool_input(&self.run_event.event);
        let (kind, text, item_id, status) = match event_type {
            "run_started" => (
                DesktopTimelineEventKind::RunStarted,
                field("prompt"),
                None,
                None,
            ),
            "text_delta" => (
                DesktopTimelineEventKind::AssistantDelta,
                field("text"),
                None,
                None,
            ),
            "reasoning_delta" => (
                DesktopTimelineEventKind::ReasoningDelta,
                field("text"),
                None,
                None,
            ),
            "assistant_message" => (
                DesktopTimelineEventKind::AssistantMessage,
                nested_string("message", "content"),
                nested_string("message", "id"),
                None,
            ),
            "tool_call_started" => (
                DesktopTimelineEventKind::ToolStarted,
                None,
                call_id.clone(),
                Some("running".to_owned()),
            ),
            "tool_call_completed" => (
                DesktopTimelineEventKind::ToolCompleted,
                None,
                call_id.clone(),
                Some("ready".to_owned()),
            ),
            "tool_progress" => (
                DesktopTimelineEventKind::ToolProgress,
                nested_string("progress", "message"),
                nested_string("progress", "call_id"),
                nested_string("progress", "status"),
            ),
            "tool_result" => (
                DesktopTimelineEventKind::ToolResult,
                nested_string("result", "content"),
                nested_string("result", "call_id"),
                tool_result_status(&self.run_event.event),
            ),
            "approval_requested" => (
                DesktopTimelineEventKind::ApprovalRequested,
                None,
                call_id.clone(),
                Some("waiting".to_owned()),
            ),
            "approval_resolved" => (
                DesktopTimelineEventKind::ApprovalResolved,
                field("reason"),
                field("call_id"),
                self.run_event
                    .event
                    .get("approved")
                    .and_then(Value::as_bool)
                    .map(|approved| if approved { "approved" } else { "denied" }.to_owned()),
            ),
            "notice" => (
                DesktopTimelineEventKind::Notice,
                field("message"),
                None,
                None,
            ),
            "usage" => (DesktopTimelineEventKind::Usage, None, None, None),
            "control" => (
                DesktopTimelineEventKind::Control,
                None,
                nested_string("control", "kind"),
                None,
            ),
            "run_finished" => (
                DesktopTimelineEventKind::RunFinished,
                field("final_text"),
                None,
                Some("finished".to_owned()),
            ),
            "run_failed" => (
                DesktopTimelineEventKind::RunFailed,
                field("error"),
                None,
                Some("failed".to_owned()),
            ),
            "run_cancelled" => (
                DesktopTimelineEventKind::RunCancelled,
                None,
                None,
                Some("cancelled".to_owned()),
            ),
            _ => (DesktopTimelineEventKind::Other, None, None, None),
        };
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
            replayable: self.event_class == DesktopProtocolEventClass::Durable,
            replay_id: self.replay_id,
            kind,
            text,
            item_id,
            tool_name,
            status,
            assistant_kind,
            tool_input,
            approval,
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
        let preview = self.run_event.event.get("preview");
        Ok(DesktopTimelineApproval {
            call_id: bounded_machine_label(&guard.call_id)?,
            tool_name: bounded_machine_label(&guard.tool_name)?,
            approval_request_id: bounded_machine_label(&guard.approval_request_id)?,
            tool_call_hash: bounded_machine_label(&guard.tool_call_hash)?,
            policy_version: bounded_machine_label(&guard.policy_version)?,
            expires_at_ms: guard.expires_at_ms,
            session_grant_available: guard.session_grant_available,
            tool_input: project_tool_input(&self.run_event.event),
            operation: self
                .run_event
                .event
                .get("operation")
                .and_then(Value::as_str)
                .map(bounded_text),
            risk: self
                .run_event
                .event
                .get("risk")
                .and_then(Value::as_str)
                .map(bounded_text),
            snapshot_required: self
                .run_event
                .event
                .get("snapshot_required")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            preview_title: preview
                .and_then(|value| value.get("title"))
                .and_then(Value::as_str)
                .map(bounded_text),
            preview_summary: preview
                .and_then(|value| value.get("summary"))
                .and_then(Value::as_str)
                .map(bounded_text),
            preview_body: preview
                .and_then(|value| value.get("body"))
                .and_then(Value::as_str)
                .map(bounded_text),
        })
    }
}

fn project_tool_input(event: &Value) -> Option<String> {
    let call = event.get("call")?;
    let tool_name = call.get("name")?.as_str()?;
    let args = serde_json::from_str::<Value>(call.get("args_json")?.as_str()?).ok()?;
    let value = match tool_name {
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

fn tool_result_status(event: &Value) -> Option<String> {
    let status = event.get("result")?.get("status")?;
    if let Some(label) = status.as_str() {
        return Some(bounded_text(label));
    }
    status
        .as_object()
        .and_then(|object| object.keys().next())
        .map(|label| bounded_text(label))
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
