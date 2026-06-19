use std::{
    collections::BTreeMap,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};

use crate::{
    ApprovalHandler, ApprovalMode, AutoApproveHandler, BackgroundTaskHandle, BackgroundTaskStatus,
    CompactionConfig, CompletionRequest, ControlEntry, EventHandler, ExternalDirectoryConfig,
    ExternalDirectoryRule, InteractionMode, JsonlSessionStore, MemoryConfig, MessageRole,
    ModelMessage, PermissionConfig, PermissionDecision, Provider, ProviderCapabilities,
    ProviderChunk, ProviderContinuationState, ReasoningArtifact, ReasoningEffort,
    ReasoningStreamSupport, ResponseHandle, RunEvent, Session, SessionLogEntry,
    TASK_PLAN_UPDATE_TOOL_NAME, TaskId, TaskPlanStatus, TaskPlanUpdateContext, TerminalTaskStatus,
    Tool, ToolAccess, ToolApproval, ToolApprovalAuditAction, ToolApprovalUserDecision, ToolCall,
    ToolCategory, ToolContext, ToolEgressAudit, ToolErrorKind, ToolExecutionStatus, ToolPreview,
    ToolPreviewCapability, ToolPreviewFile, ToolRegistry, ToolResult, ToolResultMeta, ToolSubject,
    ToolSubjectScope, UsageStats,
};

use super::{Agent, AgentRunInput, AgentRunOptions, AgentRunTerminalReason};

struct MockProvider;
struct TerminalToolProvider;

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-1".to_owned(),
                    delta: r#"{"value":"hello"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    args_json: r#"{"value":"hello"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for TerminalToolProvider {
    fn name(&self) -> &str {
        "mock-terminal"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-terminal-1".to_owned(),
                    name: "terminal_start".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-terminal-1".to_owned(),
                    delta: r#"{"command":"cargo test"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-terminal-1".to_owned(),
                    name: "terminal_start".to_owned(),
                    args_json: r#"{"command":"cargo test"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

struct CapturingTextProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for CapturingTextProvider {
    fn name(&self) -> &str {
        "mock-capturing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("captured".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ToolSideEffectProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for ToolSideEffectProvider {
    fn name(&self) -> &str {
        "mock-side-effect"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-side-effect".to_owned(),
                    name: "side_effect".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-side-effect".to_owned(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-side-effect".to_owned(),
                    name: "side_effect".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

struct EchoTool;
struct SideEffectTool;
struct TerminalStartAuditTool;
struct WriteTool {
    executed: Arc<AtomicBool>,
}
struct DefaultAllowWriteTool {
    executed: Arc<AtomicBool>,
}
struct ExternalWriteTool {
    executed: Arc<AtomicBool>,
    external_path: std::path::PathBuf,
}

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {"type": "string"}
                },
                "required": ["value"]
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "echo",
            args["value"].as_str().unwrap_or_default(),
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for SideEffectTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "side_effect".to_owned(),
            description: "returns transient context and control entries".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "side_effect",
            "side effect materialized",
            ToolResultMeta::default(),
        )
        .with_transient_context(vec![ModelMessage::system("loaded transient skill body")])
        .with_control_entry(ControlEntry::Note {
            kind: "side_effect_loaded".to_owned(),
            data: json!({"id": "repo-review"}),
        }))
    }
}

#[async_trait]
impl Tool for TerminalStartAuditTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "terminal_start".to_owned(),
            description: "Start terminal task".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Allow))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "terminal_start",
            "started terminal task terminal-1",
            ToolResultMeta {
                details: json!({
                    "task_id": "terminal-1",
                    "status": "running",
                    "status_detail": { "state": "running" },
                    "command": "cargo test",
                    "cwd": ".",
                    "shell": "sh",
                    "log_path": ".sigil/terminal/terminal-1/output.log",
                    "created_at_ms": 10,
                    "updated_at_ms": 20,
                    "output_preview": "running output",
                    "output_hash": "sha256:abc",
                    "output_truncated": false
                }),
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(vec![ToolSubject::path(path, path)])
    }

    async fn preview(
        &self,
        _ctx: ToolContext,
        args: serde_json::Value,
    ) -> Result<Option<ToolPreview>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(Some(ToolPreview {
            title: "Write file".to_owned(),
            summary: format!("Create {path}"),
            body: format!("Will write {path}"),
            changed_files: vec![path.to_owned()],
            file_diffs: vec![ToolPreviewFile {
                path: path.to_owned(),
                diff: format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1 @@\n+hello"),
            }],
        }))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executed.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "wrote file",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for DefaultAllowWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(vec![ToolSubject::path(path, path)])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Allow))
    }

    fn egress_audit(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ToolEgressAudit>> {
        Ok(Some(ToolEgressAudit {
            destination: "test:remote".to_owned(),
            operation: "write".to_owned(),
            payload: serde_json::json!({
                "argument_shape": "path-only"
            }),
            redacted: false,
        }))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executed.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "wrote file",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for ExternalWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write external".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        Ok(vec![ToolSubject::path_with_scope(
            self.external_path.display().to_string(),
            self.external_path.display().to_string(),
            Some(self.external_path.clone()),
            ToolSubjectScope::External,
        )])
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executed.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "wrote external file",
            ToolResultMeta::default(),
        ))
    }
}

struct DenyWritesHandler;

struct PanicApprovalHandler;

impl ApprovalHandler for DenyWritesHandler {
    fn approve_tool_call(
        &mut self,
        call: &ToolCall,
        _spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        Ok(ToolApproval::Deny {
            reason: format!("denied {}", call.name),
        })
    }
}

impl ApprovalHandler for PanicApprovalHandler {
    fn approve_tool_call(
        &mut self,
        _call: &ToolCall,
        _spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        panic!("approval handler should not be called")
    }
}

#[derive(Default)]
struct RecordingEventHandler {
    events: Vec<RunEvent>,
}

impl EventHandler for RecordingEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }
}

struct StateTrackingProvider;

#[async_trait]
impl Provider for StateTrackingProvider {
    fn name(&self) -> &str {
        "mock-state"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: false,
            supports_background_tasks: true,
            supports_response_handles: true,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ResponseHandle(ResponseHandle {
                provider_name: "mock-state".to_owned(),
                response_id: "response-1".to_owned(),
                continuation_cursor: Some("cursor-1".to_owned()),
            })),
            Ok(ProviderChunk::BackgroundTaskAccepted(
                BackgroundTaskHandle {
                    provider_name: "mock-state".to_owned(),
                    task_id: "task-1".to_owned(),
                    resumable: true,
                },
            )),
            Ok(ProviderChunk::BackgroundTaskStatus(BackgroundTaskStatus {
                provider_name: "mock-state".to_owned(),
                task_id: "task-1".to_owned(),
                status: "running".to_owned(),
                metadata: BTreeMap::new(),
            })),
            Ok(ProviderChunk::ContinuationState(
                ProviderContinuationState {
                    provider_name: "mock-state".to_owned(),
                    state_kind: "mock.cursor".to_owned(),
                    message_id: None,
                    opaque_blob: json!({"cursor":"next"}),
                },
            )),
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[derive(Clone)]
struct PreviousHandleRecordingProvider {
    requests: Arc<Mutex<Vec<Option<ResponseHandle>>>>,
}

impl PreviousHandleRecordingProvider {
    fn new() -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl Provider for PreviousHandleRecordingProvider {
    fn name(&self) -> &str {
        "mock-resume"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: true,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests
            .lock()
            .expect("requests mutex should not be poisoned")
            .push(request.previous_response_handle);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("resumed".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct PreviewFailingWriteTool {
    executed: Arc<AtomicBool>,
}

struct ExecuteFailingTool;
struct InvalidEgressTool;
struct PermissionAccessFailingWriteTool;
struct EgressAuditFailingWriteTool;
struct ExecuteFailingWriteTool;

#[async_trait]
impl Tool for PreviewFailingWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    async fn preview(
        &self,
        _ctx: ToolContext,
        _args: serde_json::Value,
    ) -> Result<Option<crate::ToolPreview>> {
        anyhow::bail!("preview exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executed.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "wrote file",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for PermissionAccessFailingWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(vec![ToolSubject::path(path, path)])
    }

    fn permission_access(
        &self,
        _ctx: &crate::ToolContext,
        _args: &serde_json::Value,
    ) -> Result<ToolAccess> {
        anyhow::bail!("access exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        unreachable!("tool should not execute when permission_access fails")
    }
}

#[async_trait]
impl Tool for EgressAuditFailingWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(vec![ToolSubject::path(path, path)])
    }

    fn egress_audit(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ToolEgressAudit>> {
        anyhow::bail!("egress exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        unreachable!("tool should not execute when egress audit fails")
    }
}

#[async_trait]
impl Tool for ExecuteFailingWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(vec![ToolSubject::path(path, path)])
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        anyhow::bail!("tool blew up");
    }
}

struct PreviewFallbackProvider;
struct UnknownToolProvider;
struct DirectTaskToolProvider;
struct ExecuteFailingProvider;
struct TextOnlyContinuationProvider;
struct ToolContinuationProvider;

#[async_trait]
impl Provider for PreviewFallbackProvider {
    fn name(&self) -> &str {
        "mock-preview"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ReasoningDelta("planning".to_owned())),
                Ok(ProviderChunk::ReasoningSummaryDelta(" details".to_owned())),
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-write-1".to_owned(),
                    name: "write_file".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-write-1".to_owned(),
                    delta: r#"{"path":"file.txt"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ContinuationState(
                    ProviderContinuationState {
                        provider_name: "mock-preview".to_owned(),
                        state_kind: "mock.reasoning".to_owned(),
                        message_id: None,
                        opaque_blob: json!({"reasoning":"kept"}),
                    },
                )),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-write-1".to_owned(),
                    name: "write_file".to_owned(),
                    args_json: r#"{"path":"file.txt"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for UnknownToolProvider {
    fn name(&self) -> &str {
        "mock-unknown-tool"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-missing-1".to_owned(),
                    name: "missing_tool".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for DirectTaskToolProvider {
    fn name(&self) -> &str {
        "mock-direct-task-tool"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-task-1".to_owned(),
                name: "task".to_owned(),
                args_json: r#"{"prompt":"review this"}"#.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ExecuteFailingProvider {
    fn name(&self) -> &str {
        "mock-execute-failing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-execute-1".to_owned(),
                    name: "explode".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for TextOnlyContinuationProvider {
    fn name(&self) -> &str {
        "mock-text-only"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: true,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ContinuationState(
                ProviderContinuationState {
                    provider_name: "mock-text-only".to_owned(),
                    state_kind: "mock.cursor".to_owned(),
                    message_id: None,
                    opaque_blob: json!({"cursor":"final"}),
                },
            )),
            Ok(ProviderChunk::ReasoningArtifact(ReasoningArtifact {
                provider_name: "mock-text-only".to_owned(),
                opaque_blob: json!({"ignored": true}),
            })),
            Ok(ProviderChunk::TextDelta("text only".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ToolContinuationProvider {
    fn name(&self) -> &str {
        "mock-tool-continuation"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: true,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-echo-1".to_owned(),
                name: "echo".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-echo-1".to_owned(),
                delta: r#"{"value":"hello"}"#.to_owned(),
            }),
            Ok(ProviderChunk::ContinuationState(
                ProviderContinuationState {
                    provider_name: "mock-tool-continuation".to_owned(),
                    state_kind: "mock.tool_state".to_owned(),
                    message_id: None,
                    opaque_blob: json!({"tool_call_id":"call-echo-1"}),
                },
            )),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-echo-1".to_owned(),
                name: "echo".to_owned(),
                args_json: r#"{"value":"hello"}"#.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Tool for ExecuteFailingTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "explode".to_owned(),
            description: "explode".to_owned(),
            input_schema: serde_json::json!({"type":"object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        anyhow::bail!("tool exploded");
    }
}

#[async_trait]
impl Tool for InvalidEgressTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(vec![ToolSubject::path(path, path)])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Allow))
    }

    fn egress_audit(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ToolEgressAudit>> {
        Err(anyhow::anyhow!("egress payload invalid"))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "should not execute",
            ToolResultMeta::default(),
        ))
    }
}

#[tokio::test]
async fn agent_runs_tool_then_answer() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(MockProvider, registry);
    let mut session = Session::new("mock", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let result = agent
        .run(
            &mut session,
            "hi",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;
    assert_eq!(result.final_text, "done");
    assert_eq!(result.tool_calls, 1);
    assert!(
        session
            .messages()
            .iter()
            .any(|message| message.role == MessageRole::Tool)
    );
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-1")
            && message.content.as_deref().is_some_and(|content| {
                content.contains(r#""status":"ok""#) && content.contains(r#""content":"hello""#)
            })
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-1"
                    && approval.action == ToolApprovalAuditAction::PolicyEvaluated
                    && approval.policy_decision == ApprovalMode::Allow
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Started
                    && execution.model_content_hash.is_none()
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Completed
                    && execution.model_content_hash.is_some()
                    && execution.error.is_none()
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_persists_text_before_tool_call_on_assistant_message() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::TextDelta("checking provider shape".to_owned()),
                ProviderChunk::ToolCallStart {
                    id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-1".to_owned(),
                    delta: r#"{"value":"hello"}"#.to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-1".to_owned(),
                    name: "echo".to_owned(),
                    args_json: r#"{"value":"hello"}"#.to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done".to_owned(),
        },
        registry,
    );
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut session = Session::new("mock-scripted", "mock-model").with_store(store.clone());
    let mut handler = crate::event::NoopEventHandler;

    let result = agent
        .run(
            &mut session,
            "hi",
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    let entries = JsonlSessionStore::read_entries(store.path())?;
    let assistant_tool_message = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Assistant(message) if !message.tool_calls.is_empty() => Some(message),
        _ => None,
    });
    let assistant_tool_message =
        assistant_tool_message.expect("assistant tool-call message should be persisted");
    assert_eq!(
        assistant_tool_message.content.as_deref(),
        Some("checking provider shape")
    );
    assert_eq!(assistant_tool_message.tool_calls.len(), 1);
    assert_eq!(assistant_tool_message.tool_calls[0].name, "echo");
    Ok(())
}

#[tokio::test]
async fn agent_appends_terminal_task_control_from_terminal_tool_result() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TerminalStartAuditTool));
    let agent = Agent::new(TerminalToolProvider, registry);
    let mut session = Session::new("mock-terminal", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run(
            &mut session,
            "start terminal task",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-terminal-1"
                    && execution.status == ToolExecutionStatus::Completed
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id.as_str() == "terminal-1"
                    && task.handle.command == "cargo test"
                    && matches!(task.status, TerminalTaskStatus::Running)
                    && task.output_preview.as_deref() == Some("running output")
                    && task.output_hash.as_deref() == Some("sha256:abc")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_run_input_transient_context_does_not_append_user_message() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                "transient step context",
            )]),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "captured");
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().any(|message| {
        message.role == MessageRole::User
            && message.content.as_deref() == Some("transient step context")
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::User(message)
                if message.content.as_deref() == Some("transient step context")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_run_output_reports_approval_denials() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = DenyWritesHandler;

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("write"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(output.outcome.tool_calls, 1);
    assert_eq!(output.outcome.approval_denials, 1);
    assert!(output.outcome.tool_errors.iter().any(|error| {
        error.kind == ToolErrorKind::ApprovalDenied
            && error.message.contains("tool execution denied by user")
    }));
    Ok(())
}

#[tokio::test]
async fn agent_materializes_tool_result_transient_context_and_control_entries() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(SideEffectTool));
    let agent = Agent::new(
        ToolSideEffectProvider {
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session = Session::new("mock-side-effect", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("load context"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.role == MessageRole::System
            && message.content.as_deref() == Some("loaded transient skill body")
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "side_effect_loaded" && data["id"] == "repo-review"
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::User(message)
                | SessionLogEntry::Assistant(message)
                | SessionLogEntry::ToolResult(message)
                if message.content.as_deref() == Some("loaded transient skill body")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn task_plan_update_tool_writes_plan_and_audit() -> Result<()> {
    let agent = Agent::new(PlanUpdateProvider { valid: true }, ToolRegistry::new());
    let mut session = Session::new("mock-plan", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("plan").with_task_plan_update(TaskPlanUpdateContext {
                task_id: TaskId::new("task_1")?,
                max_plan_steps: 4,
            }),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert_eq!(output.outcome.tool_errors.len(), 0);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.task_id.as_str() == "task_1"
                    && plan.plan_version == 1
                    && plan.status == TaskPlanStatus::Accepted
                    && plan.steps.len() == 1
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-plan-1"
                    && execution.tool_name == TASK_PLAN_UPDATE_TOOL_NAME
                    && execution.status == ToolExecutionStatus::Started
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-plan-1"
                    && execution.tool_name == TASK_PLAN_UPDATE_TOOL_NAME
                    && execution.status == ToolExecutionStatus::Completed
                    && execution.model_content_hash.is_some()
        )
    }));
    Ok(())
}

#[tokio::test]
async fn task_plan_update_tool_rejects_invalid_schema_without_plan_entry() -> Result<()> {
    let agent = Agent::new(PlanUpdateProvider { valid: false }, ToolRegistry::new());
    let mut session = Session::new("mock-plan", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("plan").with_task_plan_update(TaskPlanUpdateContext {
                task_id: TaskId::new("task_1")?,
                max_plan_steps: 4,
            }),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert!(output.outcome.tool_errors.iter().any(|error| {
        error.kind == ToolErrorKind::InvalidInput
            && error
                .message
                .contains("task plan must contain at least one step")
    }));
    assert!(
        !session
            .entries()
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::Control(ControlEntry::TaskPlan(_))))
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-plan-1"
                    && execution.status == ToolExecutionStatus::Failed
        )
    }));
    Ok(())
}

struct WriteMockProvider;
struct InvalidWriteArgsProvider;
struct LoopingToolProvider;
struct PlanUpdateProvider {
    valid: bool,
}

#[async_trait]
impl Provider for WriteMockProvider {
    fn name(&self) -> &str {
        "mock-write"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-write-1".to_owned(),
                    name: "write_file".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-write-1".to_owned(),
                    delta: r#"{"path":"file.txt"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-write-1".to_owned(),
                    name: "write_file".to_owned(),
                    args_json: r#"{"path":"file.txt"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for PlanUpdateProvider {
    fn name(&self) -> &str {
        "mock-plan"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }

        let args = if self.valid {
            r#"{"plan_version":1,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","role":"executor"}]}"#
        } else {
            r#"{"plan_version":1,"status":"accepted","steps":[]}"#
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-plan-1".to_owned(),
                name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-plan-1".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-plan-1".to_owned(),
                name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for InvalidWriteArgsProvider {
    fn name(&self) -> &str {
        "mock-invalid-write"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-write-1".to_owned(),
                    name: "write_file".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-write-1".to_owned(),
                    delta: r#"{"content":"missing path"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-write-1".to_owned(),
                    name: "write_file".to_owned(),
                    args_json: r#"{"content":"missing path"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for LoopingToolProvider {
    fn name(&self) -> &str {
        "mock-looping-tool"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .count()
            + 1;
        let call_id = format!("call-loop-{call_index}");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: "echo".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: r#"{"value":"again"}"#.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: "echo".to_owned(),
                args_json: r#"{"value":"again"}"#.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[tokio::test]
async fn agent_respects_denied_write_approval() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = DenyWritesHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;
    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-write-1")
            && message.content.as_deref().is_some_and(|content| {
                content.contains(r#""kind":"approval_denied""#)
                    && content.contains("tool execution denied by user")
                    && content.contains(r#""summary":"path=file.txt"#)
            })
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Requested
                    && approval.user_decision.is_none()
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Resolved
                    && approval.user_decision == Some(ToolApprovalUserDecision::Denied)
                    && approval.reason.as_deref() == Some("denied write_file")
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-write-1"
                    && execution.status == ToolExecutionStatus::Started
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_captures_tool_preview_snapshot_before_approval_request() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "write",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(executed.load(Ordering::SeqCst));

    let entries = session.entries();
    let snapshot_index = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot))
                    if snapshot.call_id == "call-write-1"
                        && snapshot.tool_name == "write_file"
                        && snapshot.file_diffs.len() == 1
                        && snapshot.file_diffs[0].path == "file.txt"
                        && snapshot.rendered_stats.added == 1
            )
        })
        .expect("preview snapshot should be captured");
    let requested_index = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                    if approval.call_id == "call-write-1"
                        && approval.action == ToolApprovalAuditAction::Requested
            )
        })
        .expect("approval request audit should be captured");
    let started_index = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == "call-write-1"
                        && execution.status == ToolExecutionStatus::Started
            )
        })
        .expect("tool execution start should be captured");

    assert!(snapshot_index < requested_index);
    assert!(requested_index < started_index);

    let snapshot_hash = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot)) => {
            snapshot.original_preview_hash.clone()
        }
        _ => None,
    });
    let approval_hash = entries.iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
            if approval.action == ToolApprovalAuditAction::Requested =>
        {
            approval.preview_hash.clone()
        }
        _ => None,
    });
    assert_eq!(snapshot_hash, approval_hash);
    let messages = session.messages();
    let tool_message_content = messages
        .iter()
        .find(|message| {
            matches!(message.role, MessageRole::Tool)
                && message.tool_call_id.as_deref() == Some("call-write-1")
        })
        .and_then(|message| message.content.as_deref())
        .expect("expected provider-visible tool message");
    assert!(!tool_message_content.contains("+hello"));
    assert!(!tool_message_content.contains("file_diffs"));
    assert!(!tool_message_content.contains("original_stats"));

    let event_snapshot_index = handler
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                RunEvent::Control(ControlEntry::ToolPreviewCaptured(snapshot))
                    if snapshot.call_id == "call-write-1"
            )
        })
        .expect("preview snapshot event should be emitted");
    let event_approval_index = handler
        .events
        .iter()
        .position(|event| matches!(event, RunEvent::ToolApprovalRequested { .. }))
        .expect("approval request event should be emitted");
    assert!(event_snapshot_index < event_approval_index);
    Ok(())
}

#[tokio::test]
async fn agent_stops_after_max_turns_without_failing_the_run() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(LoopingToolProvider, registry);
    let mut session = Session::new("mock-looping-tool", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let result = agent
        .run(
            &mut session,
            "loop",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig {
                    access: crate::PermissionAccessConfig {
                        write: Some(ApprovalMode::Allow),
                        ..crate::PermissionAccessConfig::default()
                    },
                    ..PermissionConfig::default()
                },
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "");
    assert_eq!(result.tool_calls, 2);
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(note) if note.contains("Stopped after 2 model turns"))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_returns_tool_error_when_permission_subject_is_invalid() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(InvalidWriteArgsProvider, registry);
    let mut session = Session::new("mock-invalid-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write without a path",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.content.as_deref().is_some_and(|content| {
                content.contains("invalid tool arguments for write_file")
                    && content.contains("missing string field path")
            })
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error() && result.content.contains("missing string field path"))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_returns_approval_required_in_headless_ask_mode() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Headless,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error() && result.content.contains("requires approval in headless mode"))
    }));
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-write-1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains(r#""kind":"approval_required""#))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_uses_tool_default_permission_mode() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(DefaultAllowWriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Headless,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(executed.load(Ordering::SeqCst));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::PolicyEvaluated
                    && approval.policy_decision == ApprovalMode::Allow
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolEgress(egress))
                if egress.call_id == "call-write-1"
                    && egress.tool_name == "write_file"
                    && egress.destination == "test:remote"
                    && egress.operation == "write"
                    && !egress.redacted
        )
    }));
    assert!(!handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error() && result.content.contains("requires approval in headless mode"))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_denies_write_when_subject_rule_matches() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig {
                    rules: vec![crate::PermissionRule {
                        tool_name: Some("write_file".to_owned()),
                        subject_glob: Some("file.txt".to_owned()),
                        mode: ApprovalMode::Deny,
                    }],
                    ..PermissionConfig::default()
                },
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::ToolApprovalResolved { approved: false, reason: Some(reason), .. }
                if reason.contains("permission policy")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_returns_external_directory_required_when_disabled() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("outside.txt");
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExternalWriteTool {
        executed: Arc::clone(&executed),
        external_path,
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write outside",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error()
                && matches!(result.status, crate::ToolResultStatus::Error(ref error) if error.kind == ToolErrorKind::ExternalDirectoryRequired))
    }));
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains(r#""kind":"external_directory_required""#))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_requests_approval_for_external_directory_default_ask() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_path = temp.path().canonicalize()?.join("outside.txt");
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExternalWriteTool {
        executed: Arc::clone(&executed),
        external_path: external_path.clone(),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write outside",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig {
                    external_directory: ExternalDirectoryConfig {
                        enabled: true,
                        ..ExternalDirectoryConfig::default()
                    },
                    ..PermissionConfig::default()
                },
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(executed.load(Ordering::SeqCst));
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolApprovalRequested { subjects, preview: Some(preview), .. }
            if subjects.iter().any(|subject| subject.scope == ToolSubjectScope::External)
                && preview.title.contains("External directory access")
                && preview.body.contains(&external_path.display().to_string()))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_allows_external_directory_when_all_gates_allow() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let external_root = temp.path().canonicalize()?;
    let external_path = external_root.join("outside.txt");
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExternalWriteTool {
        executed: Arc::clone(&executed),
        external_path,
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write outside",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig {
                    access: crate::PermissionAccessConfig {
                        write: Some(ApprovalMode::Allow),
                        ..crate::PermissionAccessConfig::default()
                    },
                    external_directory: ExternalDirectoryConfig {
                        enabled: true,
                        rules: vec![ExternalDirectoryRule {
                            path_glob: format!("{}/**", external_root.display()),
                            mode: ApprovalMode::Allow,
                        }],
                        ..ExternalDirectoryConfig::default()
                    },
                    ..PermissionConfig::default()
                },
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(executed.load(Ordering::SeqCst));
    assert!(
        !handler
            .events
            .iter()
            .any(|event| matches!(event, RunEvent::ToolApprovalRequested { .. }))
    );
    Ok(())
}

#[tokio::test]
async fn agent_tracks_response_handles_background_tasks_and_continuation_state() -> Result<()> {
    let registry = ToolRegistry::new();
    let agent = Agent::new(StateTrackingProvider, registry);
    let mut session = Session::new("mock-state", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let result = agent
        .run(
            &mut session,
            "continue",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::SessionLogEntry::Control(ControlEntry::ResponseHandleTracked(handle))
                if handle.response_id == "response-1"
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            crate::SessionLogEntry::Control(ControlEntry::BackgroundTaskTracked(handle))
                if handle.task_id == "task-1"
        )
    }));
    let saved_state = session.entries().iter().find_map(|entry| match entry {
        crate::SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) => Some(state),
        _ => None,
    });
    assert!(matches!(
        saved_state,
        Some(state) if state.message_id.as_deref().is_some_and(|id| !id.is_empty())
    ));
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(note) if note.contains("background task task-1 status running"))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_restores_previous_response_handle_from_durable_control_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ResponseHandleTracked(ResponseHandle {
            provider_name: "mock-resume".to_owned(),
            response_id: "response-old".to_owned(),
            continuation_cursor: Some("cursor-old".to_owned()),
        }),
    ))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ResponseHandleTracked(ResponseHandle {
            provider_name: "other-provider".to_owned(),
            response_id: "response-other".to_owned(),
            continuation_cursor: None,
        }),
    ))?;
    store.append(&SessionLogEntry::Control(
        ControlEntry::ResponseHandleTracked(ResponseHandle {
            provider_name: "mock-resume".to_owned(),
            response_id: "response-new".to_owned(),
            continuation_cursor: Some("cursor-new".to_owned()),
        }),
    ))?;
    let mut session = Session::load_from_store("mock-resume", "mock-model", store)?;
    let provider = PreviousHandleRecordingProvider::new();
    let requests = Arc::clone(&provider.requests);
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut handler = crate::event::NoopEventHandler;

    let result = agent
        .run(
            &mut session,
            "resume from control state",
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "resumed");
    let seen_requests = requests
        .lock()
        .expect("requests mutex should not be poisoned");
    assert_eq!(seen_requests.len(), 1);
    assert!(matches!(
        seen_requests[0].as_ref(),
        Some(handle) if handle.provider_name == "mock-resume"
            && handle.response_id == "response-new"
            && handle.continuation_cursor.as_deref() == Some("cursor-new")
    ));
    Ok(())
}

#[tokio::test]
async fn agent_uses_preview_fallback_and_binds_reasoning_state_to_tool_message() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PreviewFailingWriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(PreviewFallbackProvider, registry);
    let mut session = Session::new("mock-preview", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let result = agent
        .run_with_approval(
            &mut session,
            "write file",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(executed.load(Ordering::SeqCst));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::ToolApprovalRequested { preview: Some(preview), .. }
                if preview.title.contains("Preview unavailable for write_file")
                    && preview.body.contains("preview exploded")
        )
    }));
    assert!(
        handler.events.iter().any(|event| {
            matches!(event, RunEvent::ToolApprovalResolved { approved: true, .. })
        })
    );
    assert!(!handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Control(ControlEntry::ToolPreviewCaptured(snapshot))
                if snapshot.call_id == "call-write-1"
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot))
                if snapshot.call_id == "call-write-1"
        )
    }));
    let reasoning_trace_entries = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "reasoning_trace" =>
            {
                data.get("text").and_then(serde_json::Value::as_str)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reasoning_trace_entries, vec!["planning details"]);
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::Note { kind, data })
                if kind == "reasoning_delta"
                    && data.get("delta").and_then(serde_json::Value::as_str).is_some()
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-write-1"
                    && execution.status == ToolExecutionStatus::Started
                    && execution
                        .metadata
                        .details
                        .get("call")
                        .and_then(|call| call.get("summary"))
                        .and_then(serde_json::Value::as_str)
                        == Some("path=file.txt")
        )
    }));

    let assistant_tool_message_id = session
        .messages()
        .iter()
        .find(|message| !message.tool_calls.is_empty())
        .map(|message| message.id.clone());
    let saved_state = session.entries().iter().find_map(|entry| match entry {
        crate::SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) => Some(state),
        _ => None,
    });
    assert_eq!(
        saved_state.and_then(|state| state.message_id.clone()),
        assistant_tool_message_id
    );
    Ok(())
}

struct StreamErrorProvider;

#[async_trait]
impl Provider for StreamErrorProvider {
    fn name(&self) -> &str {
        "mock-stream-error"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: false,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: false,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![Err(anyhow::anyhow!(
            "socket closed"
        ))])))
    }
}

#[tokio::test]
async fn agent_returns_internal_tool_result_for_unknown_registered_name() -> Result<()> {
    let agent = Agent::new(UnknownToolProvider, ToolRegistry::new());
    let mut session = Session::new("mock-unknown-tool", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let result = agent
        .run(
            &mut session,
            "trigger unknown tool",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-missing-1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("unknown tool missing_tool"))
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-missing-1"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.error.as_ref().is_some_and(|error| error.kind == ToolErrorKind::Internal)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_guides_direct_task_tool_calls_without_hard_error() -> Result<()> {
    let agent = Agent::new(DirectTaskToolProvider, ToolRegistry::new());
    let mut session = Session::new("mock-direct-task-tool", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("delegate to a subagent"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert!(output.outcome.tool_errors.is_empty());
    assert!(session.messages().iter().any(|message| {
        matches!(message.role, MessageRole::Tool)
            && message.tool_call_id.as_deref() == Some("call-task-1")
            && message.content.as_deref().is_some_and(|content| {
                content.contains("/plan <objective>")
                    && content.contains("task_plan_update")
                    && content.contains("subagent_read")
            })
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-task-1"
                    && execution.tool_name == "task"
                    && execution.status == ToolExecutionStatus::Completed
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_records_failed_execution_when_tool_returns_error() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExecuteFailingTool));
    let agent = Agent::new(ExecuteFailingProvider, registry);
    let mut session = Session::new("mock-execute-failing", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let result = agent
        .run(
            &mut session,
            "fail execution",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error() && result.content.contains("tool exploded"))
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-execute-1"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.error.as_ref().is_some_and(|error| error.kind == ToolErrorKind::Internal)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_returns_invalid_input_when_egress_payload_audit_fails() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(InvalidEgressTool));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Headless,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error() && result.content.contains("egress payload invalid"))
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-write-1"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.error.as_ref().is_some_and(|error| error.kind == ToolErrorKind::InvalidInput)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_returns_invalid_input_when_permission_access_fails() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(PermissionAccessFailingWriteTool));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error()
                && result.content.contains("invalid tool arguments for write_file: access exploded"))
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-write-1"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.error.as_ref().is_some_and(|error| error.kind == ToolErrorKind::InvalidInput)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_returns_invalid_input_when_egress_audit_fails() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EgressAuditFailingWriteTool));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig {
                    access: crate::PermissionAccessConfig {
                        write: Some(ApprovalMode::Allow),
                        ..crate::PermissionAccessConfig::default()
                    },
                    ..PermissionConfig::default()
                },
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if result.is_error()
                && result.content.contains("invalid tool arguments for write_file: egress exploded"))
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(entry, SessionLogEntry::Control(ControlEntry::ToolEgress(egress))
            if egress.call_id == "call-write-1")
    }));
    Ok(())
}

#[tokio::test]
async fn agent_records_internal_error_when_tool_execution_fails() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExecuteFailingWriteTool));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "write something",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig {
                    access: crate::PermissionAccessConfig {
                        write: Some(ApprovalMode::Allow),
                        ..crate::PermissionAccessConfig::default()
                    },
                    ..PermissionConfig::default()
                },
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ToolResult(result)
            if matches!(&result.status, crate::ToolResultStatus::Error(error) if error.kind == ToolErrorKind::Internal)
                && result.content.contains("tool blew up"))
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-write-1"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.error.as_ref().is_some_and(|error| error.kind == ToolErrorKind::Internal)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_wraps_provider_stream_errors_with_context() {
    let agent = Agent::new(StreamErrorProvider, ToolRegistry::new());
    let mut session = Session::new("mock-stream-error", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let error = agent
        .run(
            &mut session,
            "hi",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await
        .expect_err("stream error should fail the run");

    assert!(error.to_string().contains("provider stream failed"));
    assert!(
        error
            .chain()
            .any(|cause| cause.to_string().contains("socket closed"))
    );
}

#[derive(Clone)]
struct ScriptedToolProvider {
    initial_chunks: Vec<ProviderChunk>,
    final_text: String,
}

#[async_trait]
impl Provider for ScriptedToolProvider {
    fn name(&self) -> &str {
        "mock-scripted"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            exact_prefix_cache: false,
            reports_cache_tokens: false,
            reasoning_stream: ReasoningStreamSupport::Native,
            supports_reasoning_effort: true,
            supports_tool_stream: true,
            supports_background_tasks: false,
            supports_response_handles: false,
            supports_reasoning_artifacts: true,
            supports_structured_output: false,
            supports_assistant_prefix_seed: false,
            supports_schema_constrained_tools: false,
            supports_agent_background_resume: false,
            supports_agent_thread_usage: false,
            supports_agent_result_replay: false,
            supports_infill_completion: false,
            supports_system_fingerprint: false,
            tool_name_max_chars: 64,
        }
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_used {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(self.final_text.clone())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(
                self.initial_chunks
                    .clone()
                    .into_iter()
                    .map(Ok::<_, anyhow::Error>),
            )))
        }
    }
}

struct AccessErrorTool;

struct DefaultModeErrorTool;

struct EgressAuditErrorTool;

struct ExecuteErrorTool;

fn path_tool_spec(name: &str, access: ToolAccess) -> crate::ToolSpec {
    crate::ToolSpec {
        name: name.to_owned(),
        description: name.to_owned(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"]
        }),
        category: ToolCategory::File,
        access,
        preview: ToolPreviewCapability::None,
    }
}

fn path_tool_subject(args: &serde_json::Value) -> Result<Vec<ToolSubject>> {
    let path = args
        .get("path")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
    Ok(vec![ToolSubject::path(path, path)])
}

#[async_trait]
impl Tool for AccessErrorTool {
    fn spec(&self) -> crate::ToolSpec {
        path_tool_spec("access_error", ToolAccess::Write)
    }

    fn permission_subjects(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        path_tool_subject(args)
    }

    fn permission_access(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<ToolAccess> {
        anyhow::bail!("access exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            "unreachable",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for DefaultModeErrorTool {
    fn spec(&self) -> crate::ToolSpec {
        path_tool_spec("default_mode_error", ToolAccess::Write)
    }

    fn permission_subjects(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        path_tool_subject(args)
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ApprovalMode>> {
        anyhow::bail!("default mode exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            "unreachable",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for EgressAuditErrorTool {
    fn spec(&self) -> crate::ToolSpec {
        path_tool_spec("egress_error", ToolAccess::Read)
    }

    fn permission_subjects(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        path_tool_subject(args)
    }

    fn egress_audit(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Option<ToolEgressAudit>> {
        anyhow::bail!("egress exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            "unreachable",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for ExecuteErrorTool {
    fn spec(&self) -> crate::ToolSpec {
        path_tool_spec("execute_error", ToolAccess::Read)
    }

    fn permission_subjects(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        path_tool_subject(args)
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        anyhow::bail!("tool exploded");
    }
}

#[test]
fn agent_exposes_provider_capabilities_and_mutable_tool_registry() {
    let mut agent = Agent::new(MockProvider, ToolRegistry::new());

    assert!(agent.tool_registry().specs().is_empty());
    assert!(agent.provider_capabilities().supports_tool_stream);

    agent.tool_registry_mut().register(Arc::new(EchoTool));
    assert!(agent.tool_registry().spec_for("echo").is_some());
}

#[test]
fn agent_context_helpers_attach_and_truncate_metadata() {
    let call = ToolCall {
        id: "call-ctx".to_owned(),
        name: "bash".to_owned(),
        args_json: serde_json::to_string(&json!({
            "command": format!("  echo   {}  ", "x".repeat(220)),
            "path": "notes/file.txt",
            "pattern": "needle",
        }))
        .expect("json should serialize"),
    };
    let external = std::env::temp_dir().join("outside.txt");
    let subjects = vec![
        ToolSubject::path_with_scope(
            "notes/file.txt",
            "notes/file.txt",
            Some(std::env::temp_dir().join("notes/file.txt")),
            ToolSubjectScope::Workspace,
        ),
        ToolSubject::path_with_scope(
            external.display().to_string(),
            external.display().to_string(),
            Some(external.clone()),
            ToolSubjectScope::External,
        ),
        ToolSubject::path("simple", "simple"),
        ToolSubject::command("git status --short", "git status --short"),
        ToolSubject::mcp_tool("mcp__echo"),
        ToolSubject::mcp_trust_class("server", "third_party"),
        ToolSubject::path("ignored", "ignored"),
    ];

    let context = super::tool_call_context(&call, &subjects)
        .and_then(|value| value.as_object().cloned())
        .expect("context should be derived");
    assert!(
        context["command"]
            .as_str()
            .is_some_and(|value| value.ends_with("..."))
    );
    assert_eq!(context["path"], "notes/file.txt");
    assert_eq!(context["pattern"], "needle");
    assert_eq!(context["subjects"].as_array().map(Vec::len), Some(6));
    assert!(
        context["summary"]
            .as_str()
            .is_some_and(|value| value.contains("command="))
    );

    let subject_only_context = super::tool_call_context(
        &ToolCall {
            args_json: "{}".to_owned(),
            ..call.clone()
        },
        &subjects[..1],
    )
    .expect("subjects should still yield context");
    assert!(
        subject_only_context["summary"]
            .as_str()
            .is_some_and(|value| value.starts_with("subject="))
    );

    assert!(
        super::tool_call_context(
            &ToolCall {
                args_json: "{".to_owned(),
                ..call.clone()
            },
            &[],
        )
        .is_none()
    );

    let mut null_details = ToolResult::ok("call-ctx", "bash", "ok", ToolResultMeta::default());
    super::attach_tool_call_context(&mut null_details, &call, &subjects);
    assert_eq!(
        null_details.metadata.details["call"]["path"],
        "notes/file.txt"
    );

    let mut object_details = ToolResult::ok("call-ctx", "bash", "ok", ToolResultMeta::default());
    object_details.metadata.details = json!({ "existing": true });
    super::attach_tool_call_context(&mut object_details, &call, &subjects);
    assert_eq!(object_details.metadata.details["existing"], true);
    assert_eq!(object_details.metadata.details["call"]["pattern"], "needle");

    let mut string_details = ToolResult::ok("call-ctx", "bash", "ok", ToolResultMeta::default());
    string_details.metadata.details = Value::String("previous".to_owned());
    super::attach_tool_call_context(&mut string_details, &call, &subjects);
    assert_eq!(string_details.metadata.details["tool"], "previous");
    assert_eq!(
        string_details.metadata.details["call"]["path"],
        "notes/file.txt"
    );
}

#[test]
fn agent_helper_audits_previews_and_hashes_are_structured() -> Result<()> {
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let call = ToolCall {
        id: "call-1".to_owned(),
        name: "write_file".to_owned(),
        args_json: r#"{"path":"note.txt"}"#.to_owned(),
    };
    let external_path = std::env::temp_dir().join("outside/note.txt");
    let subjects = vec![ToolSubject::path_with_scope(
        external_path.display().to_string(),
        external_path.display().to_string(),
        Some(external_path.clone()),
        ToolSubjectScope::External,
    )];
    let decision = PermissionDecision {
        access: ToolAccess::Write,
        mode: ApprovalMode::Ask,
        subjects: subjects.clone(),
        external_directory_required: true,
    };

    super::append_reasoning_trace(&mut session, "")?;
    super::append_reasoning_trace(&mut session, "trace details")?;
    let note = session
        .entries()
        .last()
        .expect("reasoning trace note should be appended");
    assert!(matches!(
        note,
        SessionLogEntry::Control(ControlEntry::Note { kind, data })
            if kind == "reasoning_trace"
                && data.get("text").and_then(serde_json::Value::as_str) == Some("trace details")
    ));

    let empty_preview = super::external_directory_preview("write_file", &[]);
    assert!(empty_preview.body.contains("No external path subjects"));
    let preview = super::external_directory_preview("write_file", &subjects);
    assert!(preview.title.contains("External directory access"));
    assert!(preview.body.contains(&external_path.display().to_string()));

    super::append_tool_approval_audit(
        &mut session,
        &call,
        &decision,
        ToolApprovalAuditAction::Requested,
        None,
        None,
        Some("preview-hash".to_owned()),
    )?;
    super::append_tool_execution_audit(
        &mut session,
        &call,
        &subjects,
        ToolExecutionStatus::Started,
        None,
        None,
    )?;
    let result = ToolResult::error(
        "call-1",
        "write_file",
        ToolErrorKind::PermissionDenied,
        "denied",
    )
    .with_error_details(true, json!({"reason": "policy"}));
    super::append_tool_execution_audit(
        &mut session,
        &call,
        &subjects,
        ToolExecutionStatus::Failed,
        Some(12),
        Some(&result),
    )?;
    session.append_control(super::tool_egress_control_entry(
        &call,
        &subjects,
        ToolEgressAudit {
            destination: "mcp:test".to_owned(),
            operation: "tools/call".to_owned(),
            payload: json!({"shape": "path-only"}),
            redacted: true,
        },
    ))?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-1"
                    && approval.preview_hash.as_deref() == Some("preview-hash")
                    && approval.external_directory_required
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Started
                    && execution.model_content_hash.is_none()
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-1"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.model_content_hash.is_some()
                    && execution.error.as_ref().is_some_and(|error| error.retryable)
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolEgress(egress))
                if egress.tool_name == "write_file"
                    && egress.destination == "mcp:test"
                    && egress.redacted
        )
    }));

    assert_eq!(super::stable_json_hash(&json!({"value": "x"}))?.len(), 64);
    assert_eq!(super::stable_text_hash("sigil").len(), 64);
    assert!(super::duration_ms(Instant::now()) < 10_000);
    Ok(())
}

#[tokio::test]
async fn agent_binds_text_only_continuation_state_to_final_assistant_message() -> Result<()> {
    let agent = Agent::new(TextOnlyContinuationProvider, ToolRegistry::new());
    let mut session = Session::new("mock-text-only", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let result = agent
        .run(
            &mut session,
            "continue",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "text only");
    let final_assistant_id = session
        .messages()
        .last()
        .map(|message| message.id.clone())
        .expect("assistant message should exist");
    let saved_state = session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) => Some(state),
        _ => None,
    });
    assert_eq!(
        saved_state.and_then(|state| state.message_id.clone()),
        Some(final_assistant_id)
    );
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::ContinuationState(state)
            if state.provider_name == "mock-text-only")
    }));
    Ok(())
}

#[tokio::test]
async fn agent_binds_tool_continuation_state_without_reasoning_to_assistant_message() -> Result<()>
{
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(ToolContinuationProvider, registry);
    let mut session = Session::new("mock-tool-continuation", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let result = agent
        .run(
            &mut session,
            "call echo",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    let assistant_tool_message_id = session
        .messages()
        .iter()
        .find(|message| !message.tool_calls.is_empty())
        .map(|message| message.id.clone());
    let saved_state = session.entries().iter().find_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ContinuationStateSaved(state)) => Some(state),
        _ => None,
    });
    assert_eq!(
        saved_state.and_then(|state| state.message_id.clone()),
        assistant_tool_message_id
    );
    Ok(())
}

#[test]
fn agent_helper_preview_and_hash_edges_cover_normalized_subjects_and_errors() -> Result<()> {
    let preview = super::external_directory_preview(
        "write_file",
        &[ToolSubject::path_with_scope(
            "outside.txt",
            "outside.txt",
            None,
            ToolSubjectScope::External,
        )],
    );
    assert!(preview.body.contains("outside.txt"));

    struct FailingSerialize;

    impl serde::Serialize for FailingSerialize {
        fn serialize<S>(&self, _serializer: S) -> std::result::Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(serde::ser::Error::custom("serialize exploded"))
        }
    }

    let error = super::stable_json_hash(&FailingSerialize).expect_err("hash should fail");
    assert!(
        error
            .to_string()
            .contains("failed to serialize audit payload")
    );

    let started = Instant::now();
    std::thread::sleep(Duration::from_millis(1));
    assert!(super::duration_ms(started) >= 1);
    Ok(())
}

#[tokio::test]
async fn agent_surfaces_invalid_permission_access_with_usage_snapshot() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(AccessErrorTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::Usage(UsageStats {
                    prompt_tokens: 7,
                    ..UsageStats::default()
                }),
                ProviderChunk::ReasoningArtifact(ReasoningArtifact {
                    provider_name: "mock-scripted".to_owned(),
                    opaque_blob: json!({"artifact": true}),
                }),
                ProviderChunk::ToolCallStart {
                    id: "call-access".to_owned(),
                    name: "access_error".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-access".to_owned(),
                    delta: r#"{"path":"note.txt"}"#.to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-access".to_owned(),
                    name: "access_error".to_owned(),
                    args_json: r#"{"path":"note.txt"}"#.to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock-scripted", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let result = agent
        .run(
            &mut session,
            "hi",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::UsageSnapshot(usage))
                if usage.prompt_tokens == 7
        )
    }));
    assert!(session.messages().iter().any(|message| {
        message.tool_call_id.as_deref() == Some("call-access")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains(r#""kind":"invalid_input""#))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_surfaces_invalid_tool_default_mode_and_egress_audit_errors() -> Result<()> {
    for (tool_name, tool) in [
        (
            "default_mode_error",
            Arc::new(DefaultModeErrorTool) as Arc<dyn Tool>,
        ),
        (
            "egress_error",
            Arc::new(EgressAuditErrorTool) as Arc<dyn Tool>,
        ),
    ] {
        let mut registry = ToolRegistry::new();
        registry.register(tool);
        let agent = Agent::new(
            ScriptedToolProvider {
                initial_chunks: vec![
                    ProviderChunk::ToolCallStart {
                        id: format!("call-{tool_name}"),
                        name: tool_name.to_owned(),
                    },
                    ProviderChunk::ToolCallArgsDelta {
                        id: format!("call-{tool_name}"),
                        delta: r#"{"path":"note.txt"}"#.to_owned(),
                    },
                    ProviderChunk::ToolCallComplete(ToolCall {
                        id: format!("call-{tool_name}"),
                        name: tool_name.to_owned(),
                        args_json: r#"{"path":"note.txt"}"#.to_owned(),
                    }),
                    ProviderChunk::Done,
                ],
                final_text: "done".to_owned(),
            },
            registry,
        );
        let mut session = Session::new("mock-scripted", "mock-model");
        let mut handler = crate::event::NoopEventHandler;

        agent
            .run(
                &mut session,
                "hi",
                AgentRunOptions {
                    workspace_root: std::env::temp_dir(),
                    max_turns: Some(4),
                    tool_timeout_secs: 5,
                    reasoning_effort: None,
                    traffic_partition_key: None,
                    interaction_mode: InteractionMode::Interactive,
                    permission_config: PermissionConfig::default(),
                    memory_config: MemoryConfig { enabled: false },
                    compaction_config: CompactionConfig::default(),
                },
                &mut handler,
            )
            .await?;

        assert!(session.messages().iter().any(|message| {
            message.tool_call_id.as_deref() == Some(&format!("call-{tool_name}"))
                && message.content.as_deref().is_some_and(|content| {
                    content.contains(r#""kind":"invalid_input""#)
                        && content.contains(if tool_name == "default_mode_error" {
                            "default mode exploded"
                        } else {
                            "egress exploded"
                        })
                })
        }));
    }
    Ok(())
}

#[tokio::test]
async fn agent_wraps_execute_errors_as_internal_tool_results() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ExecuteErrorTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "call-execute".to_owned(),
                    name: "execute_error".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-execute".to_owned(),
                    delta: r#"{"path":"note.txt"}"#.to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-execute".to_owned(),
                    name: "execute_error".to_owned(),
                    args_json: r#"{"path":"note.txt"}"#.to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock-scripted", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run(
            &mut session,
            "hi",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert!(session.messages().iter().any(|message| {
        message.tool_call_id.as_deref() == Some("call-execute")
            && message.content.as_deref().is_some_and(|content| {
                content.contains(r#""kind":"internal""#) && content.contains("tool exploded")
            })
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-execute"
                    && execution.status == ToolExecutionStatus::Failed
                    && execution.model_content_hash.is_some()
        )
    }));
    Ok(())
}
