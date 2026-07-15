use std::{
    collections::BTreeMap,
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};

use crate::{
    ApprovalHandler, ApprovalMode, AssistantMessageKind, AutoApproveHandler, BackgroundTaskHandle,
    BackgroundTaskStatus, CompactionConfig, CompletionRequest, ControlEntry, DurableEventType,
    EventHandler, ExternalDirectoryConfig, ExternalDirectoryRule, FrozenProviderRequestMaterial,
    InteractionMode, JsonlSessionStore, MemoryConfig, MessageRole, ModelMessage,
    MutationEventRecorder, PermissionConfig, PermissionDecision, PlanApprovalExpiry,
    PlanApprovalPermission, PlanApprovalScope, PlanApprovedEntry, PlanId,
    PlanPermissionGrantedEntry, PreparedToolExecution, Provider, ProviderCapabilities,
    ProviderChunk, ProviderContinuationState, ProviderPhysicalAttemptOutcome,
    ProviderPhysicalAttemptStartedEntry, ProviderPhysicalAttemptTerminalEntry,
    ProviderRequestRejection, ReasoningArtifact, ReasoningEffort, ReasoningStreamSupport,
    ResponseHandle, RunEvent, Session, SessionLogEntry, SessionStreamRecord,
    TASK_PLAN_UPDATE_TOOL_NAME, TaskId, TaskPlanStatus, TaskPlanUpdateContext, TaskRunEntry,
    TaskRunStatus, TerminalTaskStatus, Tool, ToolAccess, ToolApproval, ToolApprovalAllowSource,
    ToolApprovalAuditAction, ToolApprovalUserDecision, ToolCall, ToolCategory, ToolContext,
    ToolEgressAudit, ToolErrorKind, ToolExecutionId, ToolExecutionStatus, ToolPreparation,
    ToolPreview, ToolPreviewCapability, ToolPreviewFile, ToolProgressEvent, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSubject, ToolSubjectScope, UsageStats,
    UserUrlCapabilityRegistrar, UserUrlCapabilityRegistration, VerificationVerdict,
    VisibleCompletionState, WorkspaceMutationDetected, plan_text_hash,
};

use super::{
    Agent, AgentDelegationRequirement, AgentRunInput, AgentRunOptions, AgentRunTerminalReason,
};

struct MockProvider;
struct TerminalToolProvider;
struct TerminalCancelAfterExternalWriteProvider {
    mutation_path: PathBuf,
    calls: AtomicUsize,
}
struct NonDelegatingTextProvider {
    calls: Arc<AtomicUsize>,
}

#[derive(Default)]
struct SessionUrlRegistrarProbe {
    staged: AtomicUsize,
    committed: AtomicUsize,
    rolled_back: AtomicUsize,
}

impl UserUrlCapabilityRegistrar for SessionUrlRegistrarProbe {
    fn stage(&self, _registration: UserUrlCapabilityRegistration) -> Result<()> {
        self.staged.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn commit_message(&self, _durable_entry_id: &str) -> Result<()> {
        self.committed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn rollback_message(&self, _durable_entry_id: &str) -> Result<()> {
        self.rolled_back.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

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

#[async_trait]
impl Provider for TerminalCancelAfterExternalWriteProvider {
    fn name(&self) -> &str {
        "mock-terminal-cancel"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        match self.calls.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-terminal-start".to_owned(),
                    name: "terminal_start".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-terminal-start".to_owned(),
                    delta: r#"{"command":"sleep 5"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-terminal-start".to_owned(),
                    name: "terminal_start".to_owned(),
                    args_json: r#"{"command":"sleep 5"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ]))),
            1 => {
                std::fs::write(&self.mutation_path, "mutated\n")?;
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: "call-terminal-cancel".to_owned(),
                        name: "terminal_cancel".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: "call-terminal-cancel".to_owned(),
                        delta: r#"{"task_id":"terminal-1"}"#.to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: "call-terminal-cancel".to_owned(),
                        name: "terminal_cancel".to_owned(),
                        args_json: r#"{"task_id":"terminal-1"}"#.to_owned(),
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ]))),
        }
    }
}

#[async_trait]
impl Provider for NonDelegatingTextProvider {
    fn name(&self) -> &str {
        "mock-nondelegating"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "I will answer without delegating.".to_owned(),
            )),
            Ok(ProviderChunk::Done),
        ])))
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
struct ForegroundTerminalProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
    tool_completed: Arc<AtomicBool>,
}
struct WorkspaceMutationToolProvider;

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

#[tokio::test]
async fn agent_run_input_applies_output_token_ceiling_to_provider_request() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("bounded run").with_max_output_tokens(321),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Headless,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].max_tokens, Some(321));
    Ok(())
}

#[async_trait]
impl Provider for ForegroundTerminalProvider {
    fn name(&self) -> &str {
        "mock-foreground-terminal"
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
            if !self.tool_completed.load(Ordering::SeqCst) {
                anyhow::bail!("provider was polled before foreground terminal completed");
            }
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("foreground complete".to_owned())),
                Ok(ProviderChunk::Done),
            ])))
        } else {
            Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-terminal-foreground".to_owned(),
                    name: "terminal_start".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-terminal-foreground".to_owned(),
                    delta: r#"{"command":"cargo check 2>&1","mode":"foreground"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-terminal-foreground".to_owned(),
                    name: "terminal_start".to_owned(),
                    args_json: r#"{"command":"cargo check 2>&1","mode":"foreground"}"#.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

#[async_trait]
impl Provider for WorkspaceMutationToolProvider {
    fn name(&self) -> &str {
        "mock-workspace-mutation"
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
                    id: "call-workspace-mutation".to_owned(),
                    name: "workspace_mutation".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-workspace-mutation".to_owned(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-workspace-mutation".to_owned(),
                    name: "workspace_mutation".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])))
        }
    }
}

struct EchoTool;
struct ProgressEchoTool;
struct ForegroundTerminalTool {
    completed: Arc<AtomicBool>,
}
struct AgentCategoryTool;
struct FailingAgentCategoryTool;
struct RunningSpawnAgentCategoryTool;
struct RunningAgentCategoryTool;
struct SideEffectTool;
struct TerminalStartAuditTool;
struct TerminalCancelAuditTool;
struct WorkspaceMutatingCustomTool;
struct RecorderAwareEchoTool {
    saw_recorder: Arc<AtomicBool>,
}
struct WriteTool {
    executed: Arc<AtomicBool>,
}
struct ReadPathTool {
    executions: Arc<AtomicUsize>,
}
struct BashCargoCheckFamilyTool {
    executions: Arc<AtomicUsize>,
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
            network_effect: None,
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
impl Tool for ProgressEchoTool {
    fn spec(&self) -> crate::ToolSpec {
        EchoTool.spec()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        ctx.emit_progress(ToolProgressEvent {
            execution_id: ToolExecutionId::new("progress-echo")?,
            call_id: call_id.clone(),
            tool_name: "echo".to_owned(),
            sequence: 1,
            status: "running".to_owned(),
            message: Some("progress one".to_owned()),
            output_preview: Some("one".to_owned()),
            output_log_ref: None,
            total_bytes: Some(3),
            updated_at_ms: Some(1),
            details: json!({"phase": "one"}),
        })?;
        ctx.emit_progress(ToolProgressEvent {
            execution_id: ToolExecutionId::new("progress-echo")?,
            call_id: call_id.clone(),
            tool_name: "echo".to_owned(),
            sequence: 2,
            status: "running".to_owned(),
            message: Some("progress two".to_owned()),
            output_preview: Some("two".to_owned()),
            output_log_ref: None,
            total_bytes: Some(6),
            updated_at_ms: Some(2),
            details: json!({"phase": "two"}),
        })?;
        Ok(ToolResult::ok(
            call_id,
            "echo",
            args["value"].as_str().unwrap_or_default(),
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for ForegroundTerminalTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "terminal_start".to_owned(),
            description: "foreground terminal".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "mode": {"type": "string"}
                },
                "required": ["command"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        ctx.emit_progress(ToolProgressEvent {
            execution_id: ToolExecutionId::new("terminal-foreground")?,
            call_id: call_id.clone(),
            tool_name: "terminal_start".to_owned(),
            sequence: 1,
            status: "running".to_owned(),
            message: Some("terminal terminal-foreground running".to_owned()),
            output_preview: Some("compiling".to_owned()),
            output_log_ref: Some(PathBuf::from(
                "state/artifacts/tasks/terminal-foreground/output.log",
            )),
            total_bytes: Some(9),
            updated_at_ms: Some(1),
            details: json!({
                "task_id": "terminal-foreground",
                "status": "running",
                "execution_mode": "foreground",
                "output_preview": "compiling"
            }),
        })?;
        tokio::time::sleep(Duration::from_millis(20)).await;
        ctx.emit_progress(ToolProgressEvent {
            execution_id: ToolExecutionId::new("terminal-foreground")?,
            call_id: call_id.clone(),
            tool_name: "terminal_start".to_owned(),
            sequence: 2,
            status: "running".to_owned(),
            message: Some("terminal terminal-foreground running".to_owned()),
            output_preview: Some("finished".to_owned()),
            output_log_ref: Some(PathBuf::from(
                "state/artifacts/tasks/terminal-foreground/output.log",
            )),
            total_bytes: Some(17),
            updated_at_ms: Some(2),
            details: json!({
                "task_id": "terminal-foreground",
                "status": "running",
                "execution_mode": "foreground",
                "output_preview": "finished"
            }),
        })?;
        self.completed.store(true, Ordering::SeqCst);

        Ok(ToolResult::ok(
            call_id,
            "terminal_start",
            "terminal task terminal-foreground exited · verdict passed\nexit_code: 0\nlog: state/artifacts/tasks/terminal-foreground/output.log\noutput_preview omitted from model context; read log only if requested",
            ToolResultMeta {
                exit_code: Some(0),
                details: json!({
                    "task_id": "terminal-foreground",
                    "status": "exited",
                    "execution_mode": "foreground",
                    "verdict": "passed",
                    "rerun_not_needed": true,
                    "output_log_ref": "state/artifacts/tasks/terminal-foreground/output.log",
                    "output_preview": "finished"
                }),
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl Tool for RecorderAwareEchoTool {
    fn spec(&self) -> crate::ToolSpec {
        EchoTool.spec()
    }

    async fn execute(
        &self,
        ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.saw_recorder
            .store(ctx.mutation_recorder.is_some(), Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "echo",
            args["value"].as_str().unwrap_or_default(),
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for ReadPathTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "read_path".to_owned(),
            description: "read path".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
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
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "read_path",
            args["path"].as_str().unwrap_or_default(),
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for BashCargoCheckFamilyTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "bash".to_owned(),
            description: "bash".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(
        &self,
        _ctx: &crate::ToolContext,
        _args: &serde_json::Value,
    ) -> Result<Vec<ToolSubject>> {
        Ok(vec![ToolSubject::command(
            "family:cargo_check",
            "family:cargo_check",
        )])
    }

    fn permission_operation(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<crate::ToolOperation> {
        Ok(crate::ToolOperation::ExecuteUnknownCommand)
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "bash",
            args["command"].as_str().unwrap_or_default(),
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for AgentCategoryTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "spawn_agent".to_owned(),
            description: "spawn an agent".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
            category: ToolCategory::Agent,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "spawn_agent",
            "spawned",
            ToolResultMeta {
                details: json!({"status": "completed"}),
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl Tool for FailingAgentCategoryTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "spawn_agent".to_owned(),
            description: "spawn an agent".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
            category: ToolCategory::Agent,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::error(
            call_id,
            "spawn_agent",
            ToolErrorKind::Internal,
            "agent transport failed before a child result was available",
        ))
    }
}

#[async_trait]
impl Tool for RunningSpawnAgentCategoryTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "spawn_agent".to_owned(),
            description: "spawn an agent".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
            category: ToolCategory::Agent,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "spawn_agent",
            "agent thread started",
            ToolResultMeta {
                details: json!({
                    "agent_id": "child-1",
                    "status": "running",
                    "result_available": false
                }),
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl Tool for RunningAgentCategoryTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "wait_agent".to_owned(),
            description: "wait for an agent".to_owned(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
            category: ToolCategory::Agent,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "wait_agent",
            "agent thread is still running",
            ToolResultMeta {
                details: json!({
                    "status": "running",
                    "result_available": false
                }),
                ..ToolResultMeta::default()
            },
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
            network_effect: None,
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
impl Tool for WorkspaceMutatingCustomTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "workspace_mutation".to_owned(),
            description: "mutates the workspace through an execute-style custom tool".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Custom,
            access: ToolAccess::Execute,
            network_effect: None,
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

    async fn execute(&self, ctx: ToolContext, call_id: String, _args: Value) -> Result<ToolResult> {
        std::fs::write(ctx.workspace_root.join("mutated.txt"), "new\n")?;
        Ok(ToolResult::ok(
            call_id,
            "workspace_mutation",
            "mutated workspace",
            ToolResultMeta::default(),
        ))
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
            network_effect: None,
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
impl Tool for TerminalCancelAuditTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "terminal_cancel".to_owned(),
            description: "Cancel terminal task".to_owned(),
            input_schema: json!({"type": "object"}),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
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
            "terminal_cancel",
            "cancelled terminal task terminal-1",
            ToolResultMeta {
                details: json!({
                    "task_id": "terminal-1",
                    "status": "cancelled",
                    "status_detail": { "state": "cancelled" },
                    "command": "sleep 5",
                    "cwd": ".",
                    "shell": "sh",
                    "log_path": ".sigil/terminal/terminal-1/output.log",
                    "created_at_ms": 10,
                    "updated_at_ms": 30,
                    "output_preview": "cancelled output",
                    "output_hash": "sha256:def",
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
            network_effect: None,
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

    async fn prepare(
        &self,
        ctx: ToolContext,
        _call_id: String,
        args: serde_json::Value,
    ) -> Result<Option<ToolPreparation>> {
        let preview = self
            .preview(ctx.clone(), args.clone())
            .await?
            .ok_or_else(|| anyhow::anyhow!("write preview is required"))?;
        let subjects = self.permission_subjects(&ctx, &args)?;
        Ok(Some(ToolPreparation::new(
            preview,
            subjects,
            "sha256:test-write-artifact",
            (),
        )?))
    }

    async fn execute_prepared(
        &self,
        _ctx: ToolContext,
        _args: serde_json::Value,
        prepared: PreparedToolExecution,
    ) -> Result<ToolResult> {
        let call_id = prepared.binding().call_id.clone();
        prepared.into_artifact::<()>()?;
        self.executed.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "wrote prepared file",
            ToolResultMeta::default(),
        ))
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
            network_effect: None,
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
            network_effect: None,
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

struct ApproveForSessionHandler {
    approvals: Arc<AtomicUsize>,
}

struct ApproveWithArgsHandler;

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

impl ApprovalHandler for ApproveForSessionHandler {
    fn approve_tool_call(
        &mut self,
        _call: &ToolCall,
        _spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        self.approvals.fetch_add(1, Ordering::SeqCst);
        Ok(ToolApproval::ApproveForSession)
    }
}

impl ApprovalHandler for ApproveWithArgsHandler {
    fn approve_tool_call(
        &mut self,
        _call: &ToolCall,
        _spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        Ok(ToolApproval::ApproveWithArgs {
            args_json: r#"{"path":"changed-after-preview.txt"}"#.to_owned(),
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
            network_effect: None,
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
            network_effect: None,
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
            network_effect: None,
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
            network_effect: None,
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
            network_effect: None,
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
            network_effect: None,
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn agent_forwards_tool_progress_without_persisting_progress_as_tool_messages() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ProgressEchoTool));
    let agent = Agent::new(MockProvider, registry);
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();

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
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolProgress(_)))
            .count(),
        2
    );
    assert_eq!(
        handler
            .events
            .iter()
            .filter(
                |event| matches!(event, RunEvent::ToolResult(result) if result.tool_name == "echo")
            )
            .count(),
        1
    );
    let messages = session.messages();
    let tool_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 1);
    assert!(
        tool_messages
            .iter()
            .all(|message| message
                .content
                .as_deref()
                .is_some_and(|content| !content.contains("progress one")
                    && !content.contains("progress two")))
    );
    Ok(())
}

#[tokio::test]
async fn agent_waits_for_foreground_terminal_result_before_next_provider_request() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let completed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ForegroundTerminalTool {
        completed: Arc::clone(&completed),
    }));
    let agent = Agent::new(
        ForegroundTerminalProvider {
            captured: Arc::clone(&captured),
            tool_completed: Arc::clone(&completed),
        },
        registry,
    );
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let result = agent
        .run(
            &mut session,
            "run the workspace check",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "foreground complete");
    assert!(completed.load(Ordering::SeqCst));
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolProgress(_)))
            .count(),
        2
    );
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(
                event,
                RunEvent::ToolResult(result) if result.tool_name == "terminal_start"
            ))
            .count(),
        1
    );

    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 2);
    let second_request_tool_text = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second_request_tool_text.contains("terminal task terminal-foreground exited"));
    assert!(second_request_tool_text.contains("rerun_not_needed"));
    assert!(!second_request_tool_text.contains("compiling"));
    Ok(())
}

#[tokio::test]
async fn agent_injects_mutation_recorder_into_tool_context_for_durable_sessions() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let saw_recorder = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RecorderAwareEchoTool {
        saw_recorder: Arc::clone(&saw_recorder),
    }));
    let agent = Agent::new(MockProvider, registry);
    let mut session = Session::new("mock", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    let result = agent
        .run(
            &mut session,
            "hi",
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(saw_recorder.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn required_agent_delegation_blocks_direct_final_answer() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = NonDelegatingTextProvider {
        calls: Arc::clone(&calls),
    };
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(AgentCategoryTool));
    let agent = Agent::new(provider, registry);
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("must use a subagent").with_agent_delegation_requirement(
                AgentDelegationRequirement::new("the user explicitly requested sub-agent work"),
            ),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::DelegationUnsatisfied
    );
    assert!(output.result.final_text.is_empty());
    assert!(!session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("without delegating"))
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Notice(message)
                if message.contains("agent delegation requirement was not satisfied")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn required_agent_delegation_ignores_failed_agent_tool_before_final_answer() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(FailingAgentCategoryTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "call-agent-failed".to_owned(),
                    name: "spawn_agent".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-agent-failed".to_owned(),
                    delta: "{}".to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-agent-failed".to_owned(),
                    name: "spawn_agent".to_owned(),
                    args_json: "{}".to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done without a child result".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("must use a subagent").with_agent_delegation_requirement(
                AgentDelegationRequirement::new("the user explicitly requested sub-agent work"),
            ),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(5),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::DelegationUnsatisfied
    );
    assert!(output.result.final_text.is_empty());
    assert!(session.messages().iter().any(|message| {
        message.role == MessageRole::Tool
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("agent transport failed"))
    }));
    assert!(!session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("done without a child result"))
    }));
    Ok(())
}

#[tokio::test]
async fn required_agent_delegation_accepts_terminal_agent_tool_result() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(AgentCategoryTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "call-agent-terminal".to_owned(),
                    name: "spawn_agent".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-agent-terminal".to_owned(),
                    delta: "{}".to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-agent-terminal".to_owned(),
                    name: "spawn_agent".to_owned(),
                    args_json: "{}".to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done after terminal child result".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("must use a subagent").with_agent_delegation_requirement(
                AgentDelegationRequirement::new("the user explicitly requested sub-agent work"),
            ),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(5),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    assert_eq!(output.result.final_text, "done after terminal child result");
    assert!(session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("done after terminal child result"))
    }));
    Ok(())
}

#[tokio::test]
async fn agent_final_answer_appends_run_lifecycle_durable_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model").with_store(store.clone());
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("answer"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    let records = JsonlSessionStore::read_event_records(&path)?;
    let event_types = records
        .iter()
        .map(|record| record.stored_event().event_type.as_str())
        .collect::<Vec<_>>();
    assert!(event_types.contains(&DurableEventType::RunStatusChanged.as_str()));
    assert!(event_types.contains(&DurableEventType::RunFinalized.as_str()));
    let finalized = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str() =>
        {
            Some(event)
        }
        _ => None,
    });
    let finalized = finalized.expect("run finalized event should be present");
    assert_eq!(
        finalized.payload.get("run_status").and_then(Value::as_str),
        Some("completed")
    );
    assert_eq!(
        finalized
            .payload
            .get("terminal_reason")
            .and_then(Value::as_str),
        Some("final_answer")
    );
    assert_eq!(
        finalized
            .payload
            .get("final_message_id")
            .and_then(Value::as_str),
        output.result.final_message_id.as_deref()
    );
    let projected_entries = JsonlSessionStore::read_entries(&path)?;
    assert_eq!(projected_entries.len(), session.entries().len());
    assert_eq!(
        serde_json::to_value(&projected_entries)?,
        serde_json::to_value(session.entries())?
    );
    Ok(())
}

#[tokio::test]
async fn agent_initial_frozen_request_is_dispatched_without_rebuilding_or_duplicate_user()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model").with_store(store.clone());
    let mut safe_user = ModelMessage::user("inspect https://example.com/[redacted]");
    safe_user.id = "promoted-user".to_owned();
    session.append_user_message(safe_user)?;
    let mut exact_user = ModelMessage::user("inspect https://example.com/?signature=exact-secret");
    exact_user.id = "promoted-user".to_owned();
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-capturing".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: vec![exact_user],
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(32_768),
            reasoning_effort: Some(ReasoningEffort::High),
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: false,
            hosted_tools: Vec::new(),
        },
    )?;
    let fingerprint = frozen.fingerprint().to_owned();
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen)
                .with_logical_run_id("queued-dispatch-test"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let requests = captured.lock().expect("captured requests should lock");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].messages[0].content.as_deref(),
        Some("inspect https://example.com/?signature=exact-secret")
    );
    drop(requests);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::User(message) if message.id == "promoted-user"
            ))
            .count(),
        1
    );
    let records = JsonlSessionStore::read_event_records(&path)?;
    let started: ProviderPhysicalAttemptStartedEntry = records
        .iter()
        .find_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str() =>
            {
                serde_json::from_value(event.payload.clone()).ok()
            }
            _ => None,
        })
        .expect("frozen dispatch should append its physical-attempt Started barrier");
    assert_eq!(started.request_material_fingerprint, fingerprint);
    assert_eq!(started.logical_run_id, "queued-dispatch-test");
    let durable_json = std::fs::read_to_string(path)?;
    assert!(!durable_json.contains("exact-secret"));
    Ok(())
}

#[tokio::test]
async fn agent_initial_frozen_request_binds_only_its_first_physical_attempt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(MockProvider, registry);
    let mut session = Session::new("mock", "mock-model").with_store(store);
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: vec![ModelMessage::user("run the queued request")],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: None,
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: false,
            hosted_tools: Vec::new(),
        },
    )?;
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen)
                .with_logical_run_id("queued-dispatch-test"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let started = JsonlSessionStore::read_event_records(&path)?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str() =>
            {
                serde_json::from_value::<ProviderPhysicalAttemptStartedEntry>(event.payload).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 2);
    assert_eq!(started[0].logical_run_id, "queued-dispatch-test");
    assert_ne!(started[1].logical_run_id, "queued-dispatch-test");
    assert!(started[1].logical_run_id.starts_with("agent-run-"));
    Ok(())
}

#[tokio::test]
async fn agent_provider_turn_records_synced_physical_attempt_lifecycle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("durable provider attempt"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: Some("partition-secret".to_owned()),
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let records = JsonlSessionStore::read_event_records(&path)?;
    let started = records
        .iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let terminals = records
        .iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 1);
    assert_eq!(terminals.len(), 1);
    let started_entry: ProviderPhysicalAttemptStartedEntry =
        serde_json::from_value(started[0].payload.clone())?;
    let terminal_entry: ProviderPhysicalAttemptTerminalEntry =
        serde_json::from_value(terminals[0].payload.clone())?;
    assert!(started_entry.logical_run_id.starts_with("agent-run-"));
    assert_eq!(
        started[0].correlation_id.as_deref(),
        Some(started[0].event_id.as_str())
    );
    assert_eq!(terminals[0].correlation_id, started[0].correlation_id);
    assert_eq!(
        terminals[0].causation_id.as_deref(),
        Some(started[0].event_id.as_str())
    );
    assert_eq!(
        terminal_entry.request_material_fingerprint,
        started_entry.request_material_fingerprint
    );
    assert_eq!(
        terminal_entry.outcome,
        ProviderPhysicalAttemptOutcome::Completed
    );
    assert!(!std::fs::read_to_string(&path)?.contains("partition-secret"));
    Ok(())
}

#[tokio::test]
async fn agent_final_answer_appends_not_applicable_readiness_for_read_only_run() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("answer"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let readiness = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness)) => {
                Some(readiness)
            }
            _ => None,
        })
        .next()
        .expect("final answer should append readiness");
    assert!(matches!(
        &readiness.scope,
        crate::EvidenceScope::Run(message_id)
            if Some(message_id.as_str()) == output.result.final_message_id.as_deref()
    ));
    assert_eq!(readiness.evaluation.run_status, crate::RunStatus::Completed);
    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::NotApplicable
    );
    assert_eq!(
        readiness.evaluation.visible_state,
        VisibleCompletionState::Completed
    );
    assert!(readiness.evaluation.required_actions.is_empty());
    Ok(())
}

#[tokio::test]
async fn agent_final_answer_appends_inconclusive_readiness_for_external_process_unknown_dirty()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    MutationEventRecorder::new(store.clone()).record_external_process_unknown_dirty(
        &workspace,
        "mcp_server:docs",
        crate::ToolEffect::Unknown,
    )?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("answer"),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let readiness = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness)) => {
                Some(readiness)
            }
            _ => None,
        })
        .next()
        .expect("final answer should append readiness");
    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert_eq!(
        readiness.evaluation.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert!(
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| { matches!(action, crate::RequiredAction::ResolveUnknownDirty) })
    );
    Ok(())
}

#[tokio::test]
async fn agent_final_answer_appends_missing_readiness_after_workspace_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let store_path = temp.path().join("state/session.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WorkspaceMutatingCustomTool));
    let agent = Agent::new(WorkspaceMutationToolProvider, registry);
    let mut session = Session::new("mock-workspace-mutation", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("mutate workspace"),
            AgentRunOptions {
                workspace_root: workspace.clone(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert_eq!(
        std::fs::read_to_string(workspace.join("mutated.txt"))?,
        "new\n"
    );
    let readiness = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness)) => {
                Some(readiness)
            }
            _ => None,
        })
        .next()
        .expect("final answer should append readiness");
    assert!(matches!(
        &readiness.scope,
        crate::EvidenceScope::Run(message_id)
            if Some(message_id.as_str()) == output.result.final_message_id.as_deref()
    ));
    assert_eq!(readiness.evaluation.run_status, crate::RunStatus::Completed);
    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert_eq!(
        readiness.evaluation.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    assert!(
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| { matches!(action, crate::RequiredAction::ProvideVerificationConfig) })
    );
    let detected = JsonlSessionStore::read_event_records(&store_path)?
        .into_iter()
        .filter(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str()
            )
        })
        .count();
    assert_eq!(detected, 1);
    Ok(())
}

#[tokio::test]
async fn agent_max_turns_appends_run_lifecycle_durable_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let agent = Agent::new(MockProvider, ToolRegistry::new());
    let mut session = Session::new("mock", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("hi"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(0),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::MaxTurns
    );
    let records = JsonlSessionStore::read_event_records(&path)?;
    let finalized = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str() =>
        {
            Some(event)
        }
        _ => None,
    });
    let finalized = finalized.expect("run finalized event should be present");
    assert_eq!(
        finalized.payload.get("run_status").and_then(Value::as_str),
        Some("interrupted")
    );
    assert_eq!(
        finalized
            .payload
            .get("terminal_reason")
            .and_then(Value::as_str),
        Some("max_turns")
    );
    assert!(
        finalized
            .payload
            .get("final_message_id")
            .is_some_and(Value::is_null)
    );
    Ok(())
}

#[tokio::test]
async fn required_agent_delegation_ignores_spawn_agent_without_terminal_result() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RunningSpawnAgentCategoryTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "call-agent-started".to_owned(),
                    name: "spawn_agent".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-agent-started".to_owned(),
                    delta: "{}".to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-agent-started".to_owned(),
                    name: "spawn_agent".to_owned(),
                    args_json: "{}".to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done immediately after spawn".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("must use a subagent").with_agent_delegation_requirement(
                AgentDelegationRequirement::new("the user explicitly requested sub-agent work"),
            ),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(5),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::DelegationUnsatisfied
    );
    assert!(output.result.final_text.is_empty());
    assert!(session.messages().iter().any(|message| {
        message.role == MessageRole::Tool
            && message.tool_call_id.as_deref() == Some("call-agent-started")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("\"status\":\"running\""))
    }));
    assert!(!session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("done immediately after spawn"))
    }));
    Ok(())
}

#[tokio::test]
async fn required_agent_delegation_ignores_non_terminal_agent_tool_result() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RunningAgentCategoryTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "call-agent-running".to_owned(),
                    name: "wait_agent".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-agent-running".to_owned(),
                    delta: "{}".to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-agent-running".to_owned(),
                    name: "wait_agent".to_owned(),
                    args_json: "{}".to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done before child terminal".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("must use a subagent").with_agent_delegation_requirement(
                AgentDelegationRequirement::new("the user explicitly requested sub-agent work"),
            ),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(5),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::DelegationUnsatisfied
    );
    assert!(output.result.final_text.is_empty());
    assert!(!session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("done before child terminal"))
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
    assert_eq!(
        assistant_tool_message.assistant_kind,
        Some(AssistantMessageKind::ToolPreamble)
    );
    assert_eq!(assistant_tool_message.tool_calls.len(), 1);
    assert_eq!(assistant_tool_message.tool_calls[0].name, "echo");
    let final_message = entries.iter().rev().find_map(|entry| match entry {
        SessionLogEntry::Assistant(message) if message.tool_calls.is_empty() => Some(message),
        _ => None,
    });
    let final_message = final_message.expect("final assistant answer should be persisted");
    assert_eq!(
        final_message.assistant_kind,
        Some(AssistantMessageKind::FinalAnswer)
    );
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn agent_reconciles_terminal_start_mutation_when_terminal_cancel_finishes_task() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TerminalStartAuditTool));
    registry.register(Arc::new(TerminalCancelAuditTool));
    let agent = Agent::new(
        TerminalCancelAfterExternalWriteProvider {
            mutation_path: temp.path().join("terminal-mutated.txt"),
            calls: AtomicUsize::new(0),
        },
        registry,
    );
    let mut session = Session::new("mock-terminal-cancel", "mock-model").with_store(store.clone());
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run(
            &mut session,
            "start then cancel terminal task",
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(6),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id.as_str() == "terminal-1"
                    && matches!(task.status, TerminalTaskStatus::Cancelled)
        )
    }));
    let detected = JsonlSessionStore::read_event_records(store.path())?
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type == DurableEventType::WorkspaceMutationDetected.as_str() =>
            {
                Some(event)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(detected.len(), 1);
    let payload: WorkspaceMutationDetected = serde_json::from_value(detected[0].payload.clone())?;
    assert_eq!(payload.tool_call_id.as_deref(), Some("call-terminal-start"));
    assert_eq!(payload.tool_name, "terminal_start");
    assert!(!payload.unknown_dirty);
    assert!(payload.from_workspace_snapshot_id.is_some());
    assert!(payload.to_workspace_snapshot_id.is_some());
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn agent_run_input_preserves_consecutive_same_content_as_distinct_user_entries() -> Result<()>
{
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    session.append_user_message(ModelMessage::user("same prompt"))?;
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("same prompt"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "captured");
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::User(message)
                        if message.content.as_deref() == Some("same prompt")
                )
            })
            .count(),
        2
    );
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .filter(|message| {
                message.role == MessageRole::User
                    && message.content.as_deref() == Some("same prompt")
            })
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn safe_persistence_retry_reuses_durable_user_id_without_duplicate_append() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let input = AgentRunInput::user("same retry prompt");
    let mut handler = crate::event::NoopEventHandler;
    let options = || AgentRunOptions {
        workspace_root: std::env::temp_dir(),
        max_turns: Some(1),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    };

    agent
        .run_with_input(&mut session, input.clone(), options(), &mut handler)
        .await?;
    agent
        .run_with_input(&mut session, input, options(), &mut handler)
        .await?;

    let users = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::User(message) => Some(message),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].content.as_deref(), Some("same retry prompt"));
    Ok(())
}

#[tokio::test]
async fn safe_persistence_user_url_is_exact_once_in_request_but_never_in_session_or_snapshot()
-> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let raw_url = "https://example.com/report?token=known-secret&signature=abc";
    let prompt = format!("inspect {raw_url}");

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user(prompt.clone()),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    let durable_json = serde_json::to_string(session.entries())?;
    assert!(!durable_json.contains("known-secret"));
    assert!(!durable_json.contains("token="));
    let requests = captured
        .lock()
        .map_err(|_| anyhow::anyhow!("captured requests lock poisoned"))?;
    assert_eq!(requests.len(), 1);
    let exact_users = requests[0]
        .messages
        .iter()
        .filter(|message| {
            message.role == MessageRole::User && message.content.as_deref() == Some(prompt.as_str())
        })
        .count();
    assert_eq!(exact_users, 1);
    let snapshots = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::PrefixSnapshotCaptured(snapshot)) => {
                Some(snapshot)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!snapshots.is_empty());
    assert!(
        snapshots
            .iter()
            .all(|snapshot| !snapshot.materialized_text.contains("known-secret"))
    );
    Ok(())
}

#[tokio::test]
async fn safe_persistence_uses_session_url_registrar_across_distinct_turns_and_ownership_move()
-> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let probe = Arc::new(SessionUrlRegistrarProbe::default());
    let registrar: Arc<dyn UserUrlCapabilityRegistrar> = probe.clone();
    let mut session = Session::new("mock-capturing", "mock-model");
    session.try_attach_user_url_capability_registrar(registrar)?;
    let mut handler = crate::event::NoopEventHandler;
    let options = || AgentRunOptions {
        workspace_root: std::env::temp_dir(),
        max_turns: Some(1),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    };

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect https://example.com/a?token=one"),
            options(),
            &mut handler,
        )
        .await?;
    // The TUI moves Session into an async run and back between turns; the attachment must move
    // with it without entering serde state.
    session = std::convert::identity(session);
    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect https://example.com/b?token=two"),
            options(),
            &mut handler,
        )
        .await?;

    assert_eq!(probe.staged.load(Ordering::SeqCst), 2);
    assert_eq!(probe.committed.load(Ordering::SeqCst), 2);
    assert_eq!(probe.rolled_back.load(Ordering::SeqCst), 0);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::WebUrlCapabilityDescriptor(_))
            ))
            .count(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn safe_persistence_follow_up_request_sees_source_id_without_raw_url_material() -> Result<()>
{
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let options = || AgentRunOptions {
        workspace_root: std::env::temp_dir(),
        max_turns: Some(1),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    };
    let raw_url = "https://example.com/report?token=known-follow-up-secret";
    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user(format!("remember {raw_url}")),
            options(),
            &mut handler,
        )
        .await?;
    let source_id = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::WebUrlCapabilityDescriptor(descriptor)) => {
                Some(descriptor.source_id.clone())
            }
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("durable source descriptor missing"))?;
    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("fetch that source"),
            options(),
            &mut handler,
        )
        .await?;

    let requests = captured
        .lock()
        .map_err(|_| anyhow::anyhow!("captured requests lock poisoned"))?;
    assert_eq!(requests.len(), 2);
    let follow_up_json = serde_json::to_string(&requests[1])?;
    assert!(follow_up_json.contains(&source_id));
    assert!(follow_up_json.contains("web-source"));
    assert!(!follow_up_json.contains("known-follow-up-secret"));
    assert!(!follow_up_json.contains("token="));
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        PlanUpdateProvider {
            valid: true,
            stream_calls: Some(Arc::clone(&stream_calls)),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("plan").with_task_plan_update(TaskPlanUpdateContext {
                task_id: TaskId::new("task_1")?,
                max_plan_steps: 4,
                max_plan_versions: 1,
            }),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await?;

    assert_eq!(
        output.result.final_text,
        "task plan accepted; orchestration will continue"
    );
    assert_eq!(output.result.final_message_id, None);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
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
    let agent = Agent::new(
        PlanUpdateProvider {
            valid: false,
            stream_calls: None,
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("plan").with_task_plan_update(TaskPlanUpdateContext {
                task_id: TaskId::new("task_1")?,
                max_plan_steps: 4,
                max_plan_versions: 1,
            }),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
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
struct SessionGrantReadProvider {
    calls: Arc<AtomicUsize>,
}
struct SessionGrantCargoCheckProvider {
    calls: Arc<AtomicUsize>,
}
struct InvalidWriteArgsProvider;
struct LoopingToolProvider;
struct PlanUpdateProvider {
    valid: bool,
    stream_calls: Option<Arc<AtomicUsize>>,
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
impl Provider for SessionGrantReadProvider {
    fn name(&self) -> &str {
        "mock-session-grant-read"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        match call_index {
            0 | 2 => {
                let call_id = format!("call-read-{}", (call_index / 2) + 1);
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: call_id.clone(),
                        name: "read_path".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: call_id.clone(),
                        delta: r#"{"path":"file.txt"}"#.to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: call_id,
                        name: "read_path".to_owned(),
                        args_json: r#"{"path":"file.txt"}"#.to_owned(),
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ]))),
        }
    }
}

#[async_trait]
impl Provider for SessionGrantCargoCheckProvider {
    fn name(&self) -> &str {
        "mock-session-grant-cargo-check"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        match call_index {
            0 | 2 => {
                let call_number = (call_index / 2) + 1;
                let call_id = format!("call-cargo-{call_number}");
                let command = if call_number == 1 {
                    "cargo check 2>&1"
                } else {
                    "cd . && cargo check 2>&1 | tail -20"
                };
                let args_json = serde_json::json!({ "command": command }).to_string();
                Ok(Box::pin(stream::iter(vec![
                    Ok(ProviderChunk::ToolCallStart {
                        id: call_id.clone(),
                        name: "bash".to_owned(),
                    }),
                    Ok(ProviderChunk::ToolCallArgsDelta {
                        id: call_id.clone(),
                        delta: args_json.clone(),
                    }),
                    Ok(ProviderChunk::ToolCallComplete(ToolCall {
                        id: call_id,
                        name: "bash".to_owned(),
                        args_json,
                    })),
                    Ok(ProviderChunk::Done),
                ])))
            }
            _ => Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ]))),
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
        if let Some(stream_calls) = &self.stream_calls {
            stream_calls.fetch_add(1, Ordering::SeqCst);
        }
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

fn approved_workspace_plan(workspace_paths: Vec<&str>) -> PlanApprovedEntry {
    PlanApprovedEntry {
        plan_version: 1,
        plan_hash: plan_text_hash("approved workspace edits"),
        approved_at_ms: 42,
        permission: PlanApprovalPermission::WorkspaceEdits,
        scope: PlanApprovalScope {
            summary: "workspace edits approved for the accepted plan".to_owned(),
            workspace_paths: workspace_paths
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        },
        expires: PlanApprovalExpiry::NextUserPrompt,
        clear_planning_context: true,
    }
}

fn required_preview_file_spec(name: &str) -> crate::ToolSpec {
    crate::ToolSpec {
        name: name.to_owned(),
        description: name.to_owned(),
        input_schema: json!({"type":"object"}),
        category: ToolCategory::File,
        access: ToolAccess::Write,
        network_effect: None,
        preview: ToolPreviewCapability::Required,
    }
}

fn session_scoped_approved_workspace_plan(workspace_paths: Vec<&str>) -> PlanApprovedEntry {
    let mut approval = approved_workspace_plan(workspace_paths);
    approval.expires = PlanApprovalExpiry::Session;
    approval
}

fn task_bound_plan_permission_grant(workspace_paths: Vec<&str>) -> PlanPermissionGrantedEntry {
    PlanPermissionGrantedEntry {
        plan_id: PlanId::new("plan_test").expect("plan id"),
        plan_hash: plan_text_hash("approved workspace edits"),
        task_id: TaskId::new("task_1").expect("task id"),
        workspace_snapshot_id: Some("snapshot_1".to_owned()),
        permission: PlanApprovalPermission::WorkspaceEdits,
        scope: PlanApprovalScope {
            summary: "scoped edits for task task_1".to_owned(),
            workspace_paths: workspace_paths
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        },
        expires: PlanApprovalExpiry::Session,
        granted_at_ms: 42,
    }
}

#[test]
fn plan_approval_override_keeps_destructive_tools_behind_approval() -> Result<()> {
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanApproved(
        session_scoped_approved_workspace_plan(vec!["file.txt"]),
    ))?;

    let delete_decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "delete_file",
        ToolAccess::Write,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );
    let delete_decision = super::plan_approval_decision_override(
        &session,
        &required_preview_file_spec("delete_file"),
        delete_decision,
    );
    assert_eq!(delete_decision.mode, ApprovalMode::Ask);

    let changeset_decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "apply_changeset",
        ToolAccess::Write,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );
    let changeset_decision = super::plan_approval_decision_override(
        &session,
        &required_preview_file_spec("apply_changeset"),
        changeset_decision,
    );
    assert_eq!(changeset_decision.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn task_bound_plan_permission_grant_allows_only_scoped_file_edits() -> Result<()> {
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanPermissionGranted(
        task_bound_plan_permission_grant(vec!["file.txt"]),
    ))?;
    let in_scope = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );
    let in_scope = super::plan_approval_decision_override(
        &session,
        &required_preview_file_spec("write_file"),
        in_scope,
    );
    assert_eq!(in_scope.mode, ApprovalMode::Allow);

    let out_of_scope = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("other.txt", "other.txt")],
        false,
    );
    let out_of_scope = super::plan_approval_decision_override(
        &session,
        &required_preview_file_spec("write_file"),
        out_of_scope,
    );
    assert_eq!(out_of_scope.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn task_bound_plan_permission_grant_expires_after_task_terminal_status() -> Result<()> {
    let mut session = Session::new("mock-write", "mock-model");
    let grant = task_bound_plan_permission_grant(vec!["file.txt"]);
    session.append_control(ControlEntry::PlanPermissionGranted(grant.clone()))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: grant.task_id.clone(),
        parent_session_ref: crate::SessionRef::new_relative("session.jsonl")?,
        objective: "test task".to_owned(),
        status: TaskRunStatus::Completed,
        reason: Some("done".to_owned()),
    }))?;
    let decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );

    let decision = super::plan_approval_decision_override(
        &session,
        &required_preview_file_spec("write_file"),
        decision,
    );

    assert_eq!(decision.mode, ApprovalMode::Ask);
    Ok(())
}

#[test]
fn plan_approval_override_still_allows_ordinary_file_edits() -> Result<()> {
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanApproved(
        session_scoped_approved_workspace_plan(vec!["file.txt"]),
    ))?;
    let decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );

    let decision = super::plan_approval_decision_override(
        &session,
        &required_preview_file_spec("write_file"),
        decision,
    );

    assert_eq!(decision.mode, ApprovalMode::Allow);
    Ok(())
}

#[test]
fn prepared_authority_identities_track_durable_source_entries() -> Result<()> {
    let spec = required_preview_file_spec("write_file");
    let decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );
    let mut plan_session = Session::new("mock-write", "mock-model");
    let first_plan = session_scoped_approved_workspace_plan(vec!["file.txt"]);
    plan_session.append_control(ControlEntry::PlanApproved(first_plan.clone()))?;
    let first_authority = super::active_plan_approval_authority(&plan_session, &spec, &decision)
        .expect("first plan should authorize");
    let first_plan_identity = super::preparation_plan_approval_identity(&first_authority)?;
    let mut replacement_plan = first_plan;
    replacement_plan.approved_at_ms = replacement_plan.approved_at_ms.saturating_add(1);
    plan_session.append_control(ControlEntry::PlanApproved(replacement_plan))?;
    let replacement_authority =
        super::active_plan_approval_authority(&plan_session, &spec, &decision)
            .expect("replacement plan should authorize");
    let replacement_plan_identity =
        super::preparation_plan_approval_identity(&replacement_authority)?;
    assert_ne!(first_plan_identity, replacement_plan_identity);

    let call = ToolCall {
        id: "authority-call".to_owned(),
        name: "write_file".to_owned(),
        args_json: r#"{"path":"file.txt"}"#.to_owned(),
    };
    let prepared_digest = "sha256:interactive-authority";
    let mut interactive_session = Session::new("mock-write", "mock-model");
    assert!(
        super::resolved_interactive_approval_identity(
            &interactive_session,
            &call.id,
            prepared_digest,
        )?
        .is_none()
    );
    super::append_tool_approval_audit(
        &mut interactive_session,
        &call,
        &decision,
        ToolApprovalAuditAction::Resolved,
        Some(ToolApprovalUserDecision::Approved),
        None,
        Some(prepared_digest.to_owned()),
    )?;
    assert!(
        super::resolved_interactive_approval_identity(
            &interactive_session,
            &call.id,
            prepared_digest,
        )?
        .is_some_and(|identity| identity.starts_with("interactive:"))
    );

    let read_decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "read_path",
        ToolAccess::Read,
        vec![ToolSubject::path("file.txt", "file.txt")],
        false,
    );
    let read_call = ToolCall {
        id: "grant-call".to_owned(),
        name: "read_path".to_owned(),
        args_json: r#"{"path":"file.txt"}"#.to_owned(),
    };
    let mut grant_session = Session::new("mock-read", "mock-model");
    let mut events = RecordingEventHandler::default();
    super::append_tool_approval_session_grant(
        &mut grant_session,
        &mut events,
        &read_call,
        &read_decision,
    )?;
    let (_, first_grant) = super::tool_session_grant_decision_override(
        &grant_session,
        &read_call.name,
        read_decision.clone(),
    );
    let first_grant = first_grant.expect("first session grant should match");
    let first_grant_identity = super::preparation_session_grant_identity(&first_grant)?;
    let mut replacement_grant = first_grant;
    replacement_grant.granted_at_ms = replacement_grant.granted_at_ms.saturating_add(1);
    grant_session.append_control(ControlEntry::ToolApprovalSessionGrant(replacement_grant))?;
    let (_, replacement_grant) =
        super::tool_session_grant_decision_override(&grant_session, &read_call.name, read_decision);
    let replacement_grant = replacement_grant.expect("replacement session grant should match");
    let replacement_grant_identity = super::preparation_session_grant_identity(&replacement_grant)?;
    assert_ne!(first_grant_identity, replacement_grant_identity);
    Ok(())
}

#[test]
fn mcp_session_grant_does_not_reuse_after_exact_process_binding_changes() -> Result<()> {
    let tool_name = "mcp__same_server__echo";
    let approved_decision = PermissionDecision::new(
        ApprovalMode::Ask,
        tool_name,
        ToolAccess::Read,
        vec![ToolSubject::mcp_trust_class_with_process_binding(
            "same-server",
            "third_party",
            "hmac-sha256:approved-manifest-command-base",
            "hmac-sha256:environment",
        )],
        false,
    );
    let changed_decision = PermissionDecision::new(
        ApprovalMode::Ask,
        tool_name,
        ToolAccess::Read,
        vec![ToolSubject::mcp_trust_class_with_process_binding(
            "same-server",
            "third_party",
            "hmac-sha256:changed-manifest-command-base",
            "hmac-sha256:environment",
        )],
        false,
    );
    let call = ToolCall {
        id: "mcp-approved-for-session".to_owned(),
        name: tool_name.to_owned(),
        args_json: "{}".to_owned(),
    };
    let mut session = Session::new("mcp-grant-session", "mock-model");
    let mut events = RecordingEventHandler::default();
    super::append_tool_approval_session_grant(
        &mut session,
        &mut events,
        &call,
        &approved_decision,
    )?;

    let grant = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolApprovalSessionGrant(grant)) => Some(grant),
            _ => None,
        })
        .expect("approved-for-session fixture should append a grant");
    assert!(super::session_grant_covers_decision(
        grant,
        tool_name,
        &approved_decision
    ));
    assert!(!super::session_grant_covers_decision(
        grant,
        tool_name,
        &changed_decision
    ));
    let (changed, stale_grant) =
        super::tool_session_grant_decision_override(&session, tool_name, changed_decision);
    assert!(stale_grant.is_none());
    assert_eq!(changed.mode, ApprovalMode::Ask);
    Ok(())
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn session_grant_covers_same_stable_read_call_without_second_prompt() -> Result<()> {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadPathTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(
        SessionGrantReadProvider {
            calls: Arc::clone(&provider_calls),
        },
        registry,
    );
    let workspace = tempfile::tempdir()?;
    let run_options = || AgentRunOptions {
        workspace_root: workspace.path().to_path_buf(),
        max_turns: Some(4),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig {
            tools: BTreeMap::from([("read_path".to_owned(), ApprovalMode::Ask)]),
            ..PermissionConfig::default()
        },
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    };
    let mut session = Session::new("mock-session-grant-read", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = ApproveForSessionHandler {
        approvals: Arc::clone(&approvals),
    };

    let first = agent
        .run_with_approval(
            &mut session,
            "read file once",
            run_options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;
    let second = agent
        .run_with_approval(
            &mut session,
            "read file again",
            run_options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(first.final_text, "done");
    assert_eq!(second.final_text, "done");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 4);
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolApprovalRequested { .. }))
            .count(),
        1
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-read-1"
                    && approval.action == ToolApprovalAuditAction::Resolved
                    && approval.user_decision
                        == Some(ToolApprovalUserDecision::ApprovedForSession)
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApprovalSessionGrant(grant))
                if grant.call_id == "call-read-1"
                    && grant.tool_name == "read_path"
                    && grant.access == ToolAccess::Read
                    && grant.subjects.len() == 1
                    && grant.subjects[0].normalized == "file.txt"
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-read-2"
                    && approval.action == ToolApprovalAuditAction::Requested
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-read-2"
                    && approval.action == ToolApprovalAuditAction::PolicyEvaluated
                    && approval.policy_decision == ApprovalMode::Allow
                    && approval.allow_source == Some(ToolApprovalAllowSource::SessionGrant)
                    && approval.grant_call_id.as_deref() == Some("call-read-1")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn session_grant_covers_cargo_check_family_without_second_prompt() -> Result<()> {
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashCargoCheckFamilyTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(
        SessionGrantCargoCheckProvider {
            calls: Arc::clone(&provider_calls),
        },
        registry,
    );
    let workspace = tempfile::tempdir()?;
    let run_options = || AgentRunOptions {
        workspace_root: workspace.path().to_path_buf(),
        max_turns: Some(4),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig {
            tools: BTreeMap::from([("bash".to_owned(), ApprovalMode::Ask)]),
            ..PermissionConfig::default()
        },
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    };
    let mut session = Session::new("mock-session-grant-cargo-check", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = ApproveForSessionHandler {
        approvals: Arc::clone(&approvals),
    };

    let first = agent
        .run_with_approval(
            &mut session,
            "run cargo check",
            run_options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;
    let second = agent
        .run_with_approval(
            &mut session,
            "show cargo check tail",
            run_options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(first.final_text, "done");
    assert_eq!(second.final_text, "done");
    assert_eq!(provider_calls.load(Ordering::SeqCst), 4);
    assert_eq!(executions.load(Ordering::SeqCst), 2);
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolApprovalRequested { .. }))
            .count(),
        1
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApprovalSessionGrant(grant))
                if grant.call_id == "call-cargo-1"
                    && grant.tool_name == "bash"
                    && grant.access == ToolAccess::Execute
                    && grant.subjects.len() == 1
                    && grant.subjects[0].normalized == "family:cargo_check"
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-cargo-2"
                    && approval.action == ToolApprovalAuditAction::Requested
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-cargo-2"
                    && approval.action == ToolApprovalAuditAction::PolicyEvaluated
                    && approval.policy_decision == ApprovalMode::Allow
                    && approval.allow_source == Some(ToolApprovalAllowSource::SessionGrant)
                    && approval.grant_call_id.as_deref() == Some("call-cargo-1")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn approved_plan_workspace_edits_allows_required_preview_write_without_prompt() -> Result<()>
{
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanApproved(approved_workspace_plan(vec![
        "file.txt",
    ])))?;
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = PanicApprovalHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "execute the approved plan",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
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
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Requested
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolPreviewCaptured(snapshot))
                if snapshot.call_id == "call-write-1"
                    && snapshot.tool_name == "write_file"
                    && snapshot.file_diffs.len() == 1
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-write-1"
                    && execution.status == ToolExecutionStatus::Started
                    && execution.metadata.details["prepared_mutation"]["approval_identity"]
                        .as_str()
                        .is_some_and(|identity| identity.starts_with("plan:"))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn approved_plan_workspace_edits_requires_reapproval_for_empty_scope() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanApproved(approved_workspace_plan(
        Vec::new(),
    )))?;
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = DenyWritesHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "execute the approved plan",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Requested
        )
    }));
    Ok(())
}

#[tokio::test]
async fn approved_plan_workspace_edits_keeps_out_of_scope_write_behind_approval() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanApproved(approved_workspace_plan(vec![
        "crates/sigil-tui",
    ])))?;
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = DenyWritesHandler;

    let result = agent
        .run_with_approval(
            &mut session,
            "execute the approved plan",
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Requested
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Resolved
                    && approval.user_decision == Some(ToolApprovalUserDecision::Denied)
        )
    }));
    Ok(())
}

#[test]
fn approved_plan_next_user_prompt_expires_after_second_user_message() -> Result<()> {
    let mut session = Session::new("mock-write", "mock-model");
    session.append_control(ControlEntry::PlanApproved(approved_workspace_plan(
        Vec::new(),
    )))?;

    assert!(super::active_plan_approval(&session).is_none());
    session.append_user_message(ModelMessage::user("first prompt"))?;
    assert!(super::active_plan_approval(&session).is_some());
    session.append_user_message(ModelMessage::user("second prompt"))?;
    assert!(super::active_plan_approval(&session).is_none());
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
    let prepared_digest = snapshot_hash.expect("prepared digest should be captured");
    assert!(prepared_digest.starts_with("sha256:"));
    for approval in entries.iter().filter_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
            if approval.call_id == "call-write-1" =>
        {
            Some(approval)
        }
        _ => None,
    }) {
        assert_eq!(
            approval.preview_hash.as_deref(),
            Some(prepared_digest.as_str())
        );
    }
    for execution in entries.iter().filter_map(|entry| match entry {
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-write-1" =>
        {
            Some(execution)
        }
        _ => None,
    }) {
        assert_eq!(
            execution.metadata.details["prepared_mutation"]["prepared_digest"],
            prepared_digest
        );
        assert!(
            execution.metadata.details["prepared_mutation"]["approval_identity"]
                .as_str()
                .is_some_and(|identity| identity.starts_with("interactive:"))
        );
    }
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
async fn prepared_execution_rejects_policy_change_after_approval() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let call = ToolCall {
        id: "call-policy-change".to_owned(),
        name: "write_file".to_owned(),
        args_json: r#"{"path":"file.txt"}"#.to_owned(),
    };
    let ctx = ToolContext::new(std::env::temp_dir(), 5);
    let draft = registry
        .prepare(ctx.clone(), call.clone())
        .await?
        .expect("write tool should prepare");
    let subjects = draft.subjects().to_vec();
    let prepared = draft.bind_with_approval_identity(
        "sha256:approved-policy",
        "tool-approval:call-policy-change",
    )?;
    let result = registry
        .execute_prepared_after_started_audit(
            ctx.clone().with_approved_subjects(subjects),
            call.clone(),
            prepared,
            "sha256:changed-policy",
            "tool-approval:call-policy-change",
        )
        .await?;

    assert_eq!(
        result.summary().error_kind,
        Some(ToolErrorKind::StalePreparedMutation)
    );
    let draft = registry
        .prepare(ctx.clone(), call.clone())
        .await?
        .expect("write tool should prepare again");
    let subjects = draft.subjects().to_vec();
    let prepared =
        draft.bind_with_approval_identity("sha256:approved-policy", "plan:approved-authority")?;
    let result = registry
        .execute_prepared_after_started_audit(
            ctx.with_approved_subjects(subjects),
            call,
            prepared,
            "sha256:approved-policy",
            "plan:replacement-authority",
        )
        .await?;
    assert_eq!(
        result.summary().error_kind,
        Some(ToolErrorKind::StalePreparedMutation)
    );
    assert!(!executed.load(Ordering::SeqCst));
    Ok(())
}

#[tokio::test]
async fn prepared_execution_rejects_approval_time_argument_changes() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = ApproveWithArgsHandler;

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
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Resolved
                    && approval.user_decision == Some(ToolApprovalUserDecision::Denied)
                    && approval.reason.as_deref().is_some_and(|reason| {
                        reason.contains("approval-time argument changes")
                    })
        )
    }));
    assert!(session.messages().iter().any(|message| {
        message.tool_call_id.as_deref() == Some("call-write-1")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("stale_prepared_mutation"))
    }));
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
                    mode: crate::PermissionMode::AutoEdit,
                    ..PermissionConfig::default()
                },
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn agent_tool_default_permission_mode_cannot_relax_local_baseline() -> Result<()> {
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
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::PolicyEvaluated
                    && approval.policy_decision == ApprovalMode::Ask
                    && approval.local_policy_decision == ApprovalMode::Ask
                    && approval.source_policy_decision == ApprovalMode::Allow
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(entry, SessionLogEntry::Control(ControlEntry::ToolEgress(egress))
            if egress.call_id == "call-write-1")
    }));
    assert!(handler.events.iter().any(|event| {
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn agent_requests_approval_for_external_directory_when_disabled_interactive() -> Result<()> {
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
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
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
        matches!(event, RunEvent::ToolApprovalRequested {
            subjects,
            confirmation: Some(crate::PermissionConfirmation::TypePath),
            preview: Some(preview),
            ..
        } if subjects.iter().any(|subject| subject.scope == ToolSubjectScope::External)
            && preview.title.contains("External directory access"))
    }));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
            if approval.action == ToolApprovalAuditAction::Requested
                && approval.external_directory_required
                && approval.confirmation == Some(crate::PermissionConfirmation::TypePath)
    )));
    Ok(())
}

#[tokio::test]
async fn agent_returns_external_directory_required_when_disabled_headless() -> Result<()> {
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
                interaction_mode: InteractionMode::Headless,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
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
            && message.content.as_deref().is_some_and(|content| {
                content.contains(r#""kind":"external_directory_required""#)
                    && content.contains("$SIGIL_SCRATCH_DIR")
            })
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                    mode: crate::PermissionMode::AutoEdit,
                    tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Allow)]),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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

#[derive(Debug, thiserror::Error)]
#[error("provider rejected the request before generation")]
struct ContextWindowRejectedBeforeGeneration;

struct ContextWindowErrorProvider;
struct ContextWindowErrorAfterOutputProvider;
struct ContextWindowErrorAfterGeneratedTextProvider;

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

#[async_trait]
impl Provider for ContextWindowErrorProvider {
    fn name(&self) -> &str {
        "mock-context-window"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        error
            .downcast_ref::<ContextWindowRejectedBeforeGeneration>()
            .is_some()
            .then_some(ProviderRequestRejection::ContextWindowExceeded)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Err(ContextWindowRejectedBeforeGeneration.into())
    }
}

#[async_trait]
impl Provider for ContextWindowErrorAfterOutputProvider {
    fn name(&self) -> &str {
        "mock-context-window-after-output"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        ContextWindowErrorProvider.classify_pre_generation_rejection(error)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::Usage(UsageStats::default())),
            Err(ContextWindowRejectedBeforeGeneration.into()),
        ])))
    }
}

#[async_trait]
impl Provider for ContextWindowErrorAfterGeneratedTextProvider {
    fn name(&self) -> &str {
        "mock-context-window-after-generated-text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        ContextWindowErrorProvider.classify_pre_generation_rejection(error)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("partial output".to_owned())),
            Err(ContextWindowRejectedBeforeGeneration.into()),
        ])))
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_config: PermissionConfig {
                    tools: BTreeMap::from([("write_file".to_owned(), ApprovalMode::Allow)]),
                    ..PermissionConfig::default()
                },
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                    mode: crate::PermissionMode::AutoEdit,
                    ..PermissionConfig::default()
                },
                permission_context: crate::PermissionEvaluationContext::default(),
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
                    mode: crate::PermissionMode::AutoEdit,
                    ..PermissionConfig::default()
                },
                permission_context: crate::PermissionEvaluationContext::default(),
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
async fn agent_wraps_provider_stream_errors_with_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let agent = Agent::new(StreamErrorProvider, ToolRegistry::new());
    let mut session = Session::new("mock-stream-error", "mock-model").with_store(store);
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
    let records = JsonlSessionStore::read_event_records(&path)?;
    let finalized = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str() =>
        {
            Some(event)
        }
        _ => None,
    });
    let finalized = finalized.expect("run finalized event should be present for provider error");
    assert_eq!(
        finalized.payload.get("run_status").and_then(Value::as_str),
        Some("failed")
    );
    assert_eq!(
        finalized
            .payload
            .get("terminal_reason")
            .and_then(Value::as_str),
        Some("provider_stream_error")
    );
    assert!(
        finalized
            .payload
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error == "provider turn failed before a safe terminal result")
    );
    assert!(!finalized.payload.to_string().contains("socket closed"));
    let physical_terminal = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
        {
            serde_json::from_value::<ProviderPhysicalAttemptTerminalEntry>(event.payload.clone())
                .ok()
        }
        _ => None,
    });
    assert!(matches!(
        physical_terminal,
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn agent_persists_exact_pre_generation_context_rejection() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let agent = Agent::new(ContextWindowErrorProvider, ToolRegistry::new());
    let mut session = Session::new("mock-context-window", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    agent
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
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await
        .expect_err("context rejection should fail the current run without recovery");

    let records = JsonlSessionStore::read_event_records(&path)?;
    let terminal = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
        {
            serde_json::from_value::<ProviderPhysicalAttemptTerminalEntry>(event.payload.clone())
                .ok()
        }
        _ => None,
    });
    assert!(matches!(
        terminal,
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
            rejection: Some(ProviderRequestRejection::ContextWindowExceeded),
            durable_output_event_ids,
            durable_side_effect_event_ids,
            ..
        }) if durable_output_event_ids.is_empty() && durable_side_effect_event_ids.is_empty()
    ));
    Ok(())
}

#[tokio::test]
async fn agent_never_marks_a_rejection_after_durable_output_as_pre_generation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let agent = Agent::new(ContextWindowErrorAfterOutputProvider, ToolRegistry::new());
    let mut session =
        Session::new("mock-context-window-after-output", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    agent
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
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await
        .expect_err("a stream error after durable output should fail the current run");

    let records = JsonlSessionStore::read_event_records(&path)?;
    let terminal = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
        {
            serde_json::from_value::<ProviderPhysicalAttemptTerminalEntry>(event.payload.clone())
                .ok()
        }
        _ => None,
    });
    assert!(matches!(
        terminal,
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::FailedAfterOutputOrSideEffect,
            rejection: None,
            durable_output_event_ids,
            ..
        }) if !durable_output_event_ids.is_empty()
    ));
    Ok(())
}

#[tokio::test]
async fn agent_never_marks_a_rejection_after_observed_generation_as_pre_generation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let agent = Agent::new(
        ContextWindowErrorAfterGeneratedTextProvider,
        ToolRegistry::new(),
    );
    let mut session =
        Session::new("mock-context-window-after-generated-text", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    agent
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
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig { enabled: false },
                compaction_config: CompactionConfig::default(),
            },
            &mut handler,
        )
        .await
        .expect_err("a rejection after generated text should fail the current run");

    let records = JsonlSessionStore::read_event_records(&path)?;
    let terminal = records.iter().find_map(|record| match record {
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
        {
            serde_json::from_value::<ProviderPhysicalAttemptTerminalEntry>(event.payload.clone())
                .ok()
        }
        _ => None,
    });
    assert!(matches!(
        terminal,
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput,
            rejection: None,
            durable_output_event_ids,
            durable_side_effect_event_ids,
            ..
        }) if durable_output_event_ids.is_empty() && durable_side_effect_event_ids.is_empty()
    ));
    Ok(())
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
        network_effect: None,
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
    let decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        subjects.clone(),
        true,
    );

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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
                    permission_context: crate::PermissionEvaluationContext::default(),
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
                permission_context: crate::PermissionEvaluationContext::default(),
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
