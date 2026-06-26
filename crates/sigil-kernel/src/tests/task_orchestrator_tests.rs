use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, AutoApproveHandler, CandidateCheck, CheckCommand,
    CheckDiscoverySource, CheckPromotion, CheckSpecRecordedEntry, CompletionRequest, ControlEntry,
    DurableEventType, EventClass, EvidenceScope, InteractionMode, JsonlSessionStore, MemoryConfig,
    MessageRole, ModelMessage, PermissionConfig, Provider, ProviderCapabilities, ProviderChunk,
    ReasoningEffort, ReasoningStreamSupport, RunEvent, SequentialTaskOrchestrator,
    SequentialTaskRequest, Session, SessionLogEntry, SessionRef, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildSessionStatus, TaskId, TaskPlanEntry, TaskPlanStatus, TaskRouteStatus, TaskRunStatus,
    TaskStepId, TaskStepSpec, TaskStepStatus, Tool, ToolAccess, ToolApproval, ToolCall,
    ToolCategory, ToolContext, ToolEffect, ToolPreviewCapability, ToolRegistry, ToolResult,
    ToolResultMeta, ToolSpec, VerificationVerdict, VisibleCompletionState, WorkspaceTrust,
    WorkspaceTrustDecisionEntry, write_file_with_mutation,
};

use super::{
    StepRunOutput, child_status_from_output, durable_workspace_mutation_evidence, planner_prompt,
    route_id_for_call, run_status_from_step_status, run_task_step_verification_checks,
    step_status_after_readiness, step_status_from_outcome, step_terminal_reason,
    task_status_from_step_status, task_step_readiness,
};

struct PlannerProvider;
struct NoPlanProvider;
struct FailingProvider;
struct ToolCallingProvider;
struct MutatingToolProvider;
struct RecoveringToolErrorProvider;
struct RecoverableErrorTool;
struct MutatingTool;
struct ApprovalRequiredTool;
struct DenyApprovalHandler;
#[derive(Default)]
struct RecordingEventHandler {
    events: Vec<RunEvent>,
}

impl crate::EventHandler for RecordingEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }
}

#[test]
fn planner_prompt_explains_subagent_delegation_without_direct_task_tool() {
    let prompt = planner_prompt("review implementation");

    assert!(prompt.contains("Do not call a task or subagent tool"));
    assert!(prompt.contains("role subagent_read or subagent_write"));
    assert!(prompt.contains("child sessions"));
}

#[test]
fn new_with_child_runner_constructs_orchestrator() {
    let _orchestrator = SequentialTaskOrchestrator::new_with_child_runner(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        crate::LegacyTaskChildSessionRunner::new(
            boxed_agent(
                CapturingExecutorProvider {
                    requests: Arc::new(Mutex::new(Vec::new())),
                },
                ToolRegistry::new(),
            ),
            boxed_agent(
                CapturingExecutorProvider {
                    requests: Arc::new(Mutex::new(Vec::new())),
                },
                ToolRegistry::new(),
            ),
        ),
    );
}

struct CapturingExecutorProvider {
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for PlannerProvider {
    fn name(&self) -> &str {
        "planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("call-mutate-1"));
        if tool_used {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("planned".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = r#"{"plan_version":1,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect code","role":"executor"}]}"#;
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
impl Provider for NoPlanProvider {
    fn name(&self) -> &str {
        "planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("no plan".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for FailingProvider {
    fn name(&self) -> &str {
        "failing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Err(anyhow::anyhow!("provider failed"))
    }
}

#[async_trait]
impl Provider for CapturingExecutorProvider {
    fn name(&self) -> &str {
        "executor"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .push(request);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("step complete".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ToolCallingProvider {
    fn name(&self) -> &str {
        "tool-calling"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
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
                Ok(ProviderChunk::TextDelta("tool step done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = r#"{"path":"note.txt"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-write-1".to_owned(),
                name: "write_file".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-write-1".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-write-1".to_owned(),
                name: "write_file".to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for MutatingToolProvider {
    fn name(&self) -> &str {
        "mutating-tool"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_used = request
            .messages
            .iter()
            .any(|message| message.tool_call_id.as_deref() == Some("call-mutate-1"));
        if tool_used {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("mutation verified".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = r#"{"path":"note.txt"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-mutate-1".to_owned(),
                name: "mutate_file".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-mutate-1".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-mutate-1".to_owned(),
                name: "mutate_file".to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RecoveringToolErrorProvider {
    fn name(&self) -> &str {
        "recovering-tool-error"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_message_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool));
        if tool_message_seen {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("recovered step".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = r#"{"path":"bad.txt"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-recoverable-error".to_owned(),
                name: "recoverable_error".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-recoverable-error".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-recoverable-error".to_owned(),
                name: "recoverable_error".to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Tool for RecoverableErrorTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "recoverable_error".to_owned(),
            description: "recoverable read error".to_owned(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            category: ToolCategory::File,
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
        Ok(ToolResult::error(
            call_id,
            "recoverable_error",
            crate::ToolErrorKind::InvalidInput,
            "bad path",
        ))
    }
}

#[async_trait]
impl Tool for MutatingTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "mutate_file".to_owned(),
            description: "Mutate a test file".to_owned(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Optional,
        }
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("note.txt")
            .to_owned();
        std::fs::write(ctx.workspace_root.join(&path), "new\n")?;
        Ok(ToolResult::ok(
            call_id,
            "mutate_file",
            "mutated",
            ToolResultMeta {
                changed_files: vec![path],
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl Tool for ApprovalRequiredTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: "approval required write".to_owned(),
            input_schema: json!({"type":"object","properties":{"path":{"type":"string"}}}),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<crate::ApprovalMode>> {
        Ok(Some(crate::ApprovalMode::Ask))
    }

    async fn preview(&self, _ctx: ToolContext, _args: Value) -> Result<Option<crate::ToolPreview>> {
        Ok(Some(crate::ToolPreview {
            title: "Write note.txt".to_owned(),
            summary: "Update note.txt".to_owned(),
            body: "--- current/note.txt\n+++ proposed/note.txt\n@@ -0,0 +1 @@\n+test".to_owned(),
            changed_files: vec!["note.txt".to_owned()],
            file_diffs: Vec::new(),
        }))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "written",
            ToolResultMeta::default(),
        ))
    }
}

impl crate::ApprovalHandler for DenyApprovalHandler {
    fn approve_tool_call(&mut self, _call: &ToolCall, _spec: &ToolSpec) -> Result<ToolApproval> {
        Ok(ToolApproval::Deny {
            reason: "blocked in test".to_owned(),
        })
    }
}

#[tokio::test]
async fn sequential_task_orchestrator_runs_plan_and_executor_step() -> Result<()> {
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&executor_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            options(),
            4,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.plan_version, 1);
    assert_eq!(output.steps.len(), 1);
    assert_eq!(output.steps[0].status, TaskStepStatus::Completed);
    assert_eq!(
        output.steps[0].verification_verdict,
        VerificationVerdict::NotApplicable
    );
    assert_eq!(
        output.steps[0].visible_state,
        VisibleCompletionState::Completed
    );
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Started
        )
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Control(ControlEntry::TaskStep(step))
                if step.step_id.as_str() == "step_1"
                    && step.status == TaskStepStatus::Running
        )
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Control(ControlEntry::TaskStep(step))
                if step.step_id.as_str() == "step_1"
                    && step.status == TaskStepStatus::Completed
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Completed
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.status == TaskStepStatus::Completed
                    && step.summary.as_deref() == Some("step complete")
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness))
                if readiness.evaluation.run_status == crate::RunStatus::Completed
                    && readiness.evaluation.verification_verdict
                        == VerificationVerdict::NotApplicable
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::User(message)
                if message.content.as_deref().is_some_and(|content| {
                    content.contains("Execute task step")
                })
        )
    }));
    let requests = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().any(|message| {
        message.role == MessageRole::User
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("Execute task step"))
    }));
    Ok(())
}

#[tokio::test]
async fn sequential_task_orchestrator_runs_configured_check_after_mutating_step() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("planner", "model").with_store(store);
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            trusted,
            "event-config",
        ),
    ))?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(MutatingToolProvider, registry),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut options = options();
    options.workspace_root = workspace.clone();
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "mutate and verify".to_owned(),
            },
            options.clone(),
            options.clone(),
            options.clone(),
            options,
            4,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.steps[0].status, TaskStepStatus::Completed);
    assert_eq!(
        output.steps[0].verification_verdict,
        VerificationVerdict::Passed
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("note.txt"))?,
        "new\n"
    );
    assert!(
        session
            .verification_state_projection()
            .receipts
            .values()
            .any(|entry| entry.receipt.check_status == crate::ReceiptStatus::Succeeded)
    );
    Ok(())
}

#[tokio::test]
async fn sequential_task_orchestrator_blocks_mutating_step_without_verification_config()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("planner", "model").with_store(store);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(MutatingToolProvider, registry),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut options = options();
    options.workspace_root = workspace.clone();
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "mutate without verification config".to_owned(),
            },
            options.clone(),
            options.clone(),
            options.clone(),
            options,
            4,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Paused);
    assert_eq!(output.steps[0].status, TaskStepStatus::Blocked);
    assert_eq!(
        output.steps[0].verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness))
                if readiness.evaluation.run_status == crate::RunStatus::Blocked
                    && readiness
                        .evaluation
                        .required_actions
                        .contains(&crate::RequiredAction::ProvideVerificationConfig)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn continue_run_skips_completed_steps_and_executes_remaining() -> Result<()> {
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&executor_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    seed_two_step_task(&mut session, TaskRunStatus::Paused, true)?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            Some("focus runtime state updates".to_owned()),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.plan_version, 1);
    assert_eq!(output.steps.len(), 1);
    assert_eq!(output.steps[0].step_id, TaskStepId::new("step_2")?);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Running
                    && run.reason.as_deref().is_some_and(|reason| {
                        reason.contains("focus runtime state updates")
                    })
        )
    }));
    let requests = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("Step: step_2")
                && content.contains("User guidance for this continuation")
                && content.contains("focus runtime state updates")
        })
    }));
    Ok(())
}

#[tokio::test]
async fn continue_run_continues_after_recovered_tool_error() -> Result<()> {
    let mut executor_registry = ToolRegistry::new();
    executor_registry.register(Arc::new(RecoverableErrorTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(RecoveringToolErrorProvider, executor_registry),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    seed_two_step_task(&mut session, TaskRunStatus::Paused, false)?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.steps.len(), 2);
    assert!(
        output
            .steps
            .iter()
            .all(|step| step.status == TaskStepStatus::Completed)
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskStep(step))
                    if step.status == TaskStepStatus::Completed
            ))
            .count(),
        2
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.step_id == TaskStepId::new("step_1").expect("valid step id")
                    && step.reason.as_deref().is_some_and(|reason| {
                        reason.contains("recovered tool error")
                            && reason.contains("bad path")
                    })
        )
    }));
    Ok(())
}

#[tokio::test]
async fn continue_run_errors_when_task_is_missing() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let error = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("missing_task")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await
        .expect_err("missing task should fail");

    assert!(error.to_string().contains("missing_task"));
    Ok(())
}

#[tokio::test]
async fn planner_provider_error_marks_task_failed() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(FailingProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let result = orchestrator
        .run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            options(),
            4,
            &mut handler,
            &mut approval_handler,
        )
        .await;

    assert!(result.is_err());
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Failed
                    && run
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("planner failed"))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn planner_role_step_runs_on_parent_executor_path() -> Result<()> {
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&executor_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    seed_single_step_task(&mut session, crate::AgentRole::Planner)?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    let requests = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned");
    assert!(requests[0].messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Role: planner"))
    }));
    Ok(())
}

#[tokio::test]
async fn subagent_step_runs_in_child_session_and_links_parent() -> Result<()> {
    let subagent_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&subagent_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    session.append_control(ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "delegate read".to_owned(),
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step_1")?,
            title: "read in child".to_owned(),
            display_name: None,
            detail: None,
            role: crate::AgentRole::SubagentRead,
        }],
        reason: None,
    }))?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "delegate read".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Started
                    && child.role == crate::AgentRole::SubagentRead
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Completed
                    && child.summary_hash.is_some()
        )
    }));
    let requests = subagent_requests
        .lock()
        .expect("subagent request lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("delegated subagent step"))
    }));
    Ok(())
}

#[tokio::test]
async fn direct_child_session_keeps_skill_context_out_of_parent_history() -> Result<()> {
    let subagent_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&subagent_requests),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_skill")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "invoke skill".to_owned(),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill")?,
                title: "invoke skill repo-review".to_owned(),
                display_name: None,
                detail: Some("direct user-invoked child-session skill".to_owned()),
                role: crate::AgentRole::SubagentWrite,
            },
            AgentRunInput::without_persisted_user_message(vec![
                ModelMessage::system("Loaded Sigil skill body SECRET_SKILL_BODY"),
                ModelMessage::user("Apply the loaded skill"),
            ]),
            options(),
            options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Completed
                    && child.role == crate::AgentRole::SubagentWrite
        )
    }));
    assert!(
        !session
            .entries()
            .iter()
            .any(|entry| matches!(entry, SessionLogEntry::User(_)))
    );
    let parent_entries = format!("{:?}", session.entries());
    assert!(!parent_entries.contains("SECRET_SKILL_BODY"));

    let requests = subagent_requests
        .lock()
        .expect("subagent request lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    let subagent_messages = format!("{:?}", requests[0].messages);
    assert!(subagent_messages.contains("SECRET_SKILL_BODY"));
    assert!(subagent_messages.contains("Apply the loaded skill"));
    Ok(())
}

#[tokio::test]
async fn direct_child_session_runs_configured_check_after_mutating_write() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("planner", "model").with_store(store);
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_skill".to_owned()),
            trusted,
            "event-config",
        ),
    ))?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(MutatingToolProvider, registry),
    );
    let mut options = options();
    options.workspace_root = workspace.clone();
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_skill")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "invoke write skill".to_owned(),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill")?,
                title: "invoke write skill".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentWrite,
            },
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("write")]),
            options.clone(),
            options,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.steps[0].status, TaskStepStatus::Completed);
    assert_eq!(
        output.steps[0].verification_verdict,
        VerificationVerdict::Passed
    );
    assert_eq!(
        std::fs::read_to_string(workspace.join("note.txt"))?,
        "new\n"
    );
    assert!(
        session
            .verification_state_projection()
            .receipts
            .values()
            .any(|entry| entry.receipt.check_status == crate::ReceiptStatus::Succeeded)
    );
    Ok(())
}

#[tokio::test]
async fn direct_child_session_blocks_mutating_write_without_verification_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("planner", "model").with_store(store);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(MutatingToolProvider, registry),
    );
    let mut options = options();
    options.workspace_root = workspace;
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_skill")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "invoke write skill".to_owned(),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill")?,
                title: "invoke write skill".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentWrite,
            },
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("write")]),
            options.clone(),
            options,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Paused);
    assert_eq!(output.steps[0].status, TaskStepStatus::Blocked);
    assert_eq!(
        output.steps[0].verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness))
                if readiness.evaluation.run_status == crate::RunStatus::Blocked
        )
    }));
    Ok(())
}

#[tokio::test]
async fn direct_child_session_rejects_non_subagent_roles() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let error = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_skill")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "invoke skill".to_owned(),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill")?,
                title: "invoke skill".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
            },
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("run")]),
            options(),
            options(),
            &mut handler,
            &mut approval_handler,
        )
        .await
        .expect_err("non-subagent role should be rejected");

    assert!(error.to_string().contains("requires a subagent role"));
    assert!(session.entries().is_empty());
    Ok(())
}

#[tokio::test]
async fn direct_child_session_supports_subagent_read_role() -> Result<()> {
    let subagent_read_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&subagent_read_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_read_skill")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "invoke read skill".to_owned(),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill")?,
                title: "invoke read skill".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentRead,
            },
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("inspect")]),
            options(),
            options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(
        subagent_read_requests
            .lock()
            .expect("subagent read requests should not be poisoned")
            .len(),
        1
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.role == crate::AgentRole::SubagentRead
                    && child.status == TaskChildSessionStatus::Completed
        )
    }));
    Ok(())
}

#[tokio::test]
async fn direct_child_session_records_failed_child_provider() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(FailingProvider, ToolRegistry::new()),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_failing_skill")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "invoke failing skill".to_owned(),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("invoke_skill")?,
                title: "invoke failing skill".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentWrite,
            },
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("run")]),
            options(),
            options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Failed);
    assert_eq!(output.steps[0].status, TaskStepStatus::Failed);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Failed
                    && run.reason.as_deref().is_some_and(|reason| reason.contains("provider failed"))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn subagent_write_step_routes_denied_approval_to_parent_session() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session-parent.jsonl"))?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ApprovalRequiredTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(ToolCallingProvider, registry),
    );
    let mut session = Session::load_from_store("planner", "model", store)?;
    seed_single_step_task(&mut session, crate::AgentRole::SubagentWrite)?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = DenyApprovalHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "delegate write".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Paused);
    assert_eq!(output.steps[0].status, TaskStepStatus::Blocked);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(route))
                if route.status == TaskRouteStatus::Requested
                    && route.tool_name == "write_file"
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(route))
                if route.status == TaskRouteStatus::Rejected
                    && route.call_id == "call-write-1"
        )
    }));
    assert!(temp.path().join("children/task_1").is_dir());
    Ok(())
}

#[tokio::test]
async fn subagent_write_step_routes_approved_approval_to_parent_session() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ApprovalRequiredTool));
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(ToolCallingProvider, registry),
    );
    let mut session = Session::new("planner", "model");
    seed_single_step_task(&mut session, crate::AgentRole::SubagentWrite)?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "delegate write".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(route))
                if route.status == TaskRouteStatus::Resolved
                    && route.call_id == "call-write-1"
        )
    }));
    Ok(())
}

#[tokio::test]
async fn child_step_rejects_parent_role_in_child_runner() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "delegate through fallback".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "fallback".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let result = orchestrator
        .run_child_step(
            &mut session,
            &request,
            1,
            &step,
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await;

    assert!(result.is_err());
    let error = result.err().expect("error was checked above");
    assert!(
        error
            .to_string()
            .contains("task child session runner requires a subagent role")
    );
    Ok(())
}

#[tokio::test]
async fn subagent_step_error_marks_child_session_failed() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(FailingProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    session.append_control(ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "delegate read".to_owned(),
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step_1")?,
            title: "read in child".to_owned(),
            display_name: None,
            detail: None,
            role: crate::AgentRole::SubagentRead,
        }],
        reason: None,
    }))?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "delegate read".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Failed);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Started
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Failed
        )
    }));
    Ok(())
}

#[tokio::test]
async fn max_turns_marks_step_and_task_interrupted() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    seed_two_step_task(&mut session, TaskRunStatus::Paused, true)?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;
    let mut executor_options = options();
    executor_options.max_turns = Some(0);

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            executor_options,
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Interrupted);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.step_id == TaskStepId::new("step_2").expect("valid step id")
                    && step.status == TaskStepStatus::Interrupted
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Interrupted
        )
    }));
    Ok(())
}

#[tokio::test]
async fn planner_without_plan_marks_task_failed() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(NoPlanProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let result = orchestrator
        .run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            options(),
            4,
            &mut handler,
            &mut approval_handler,
        )
        .await;

    assert!(result.is_err());
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Failed
                    && run
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("task orchestration failed"))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn proposed_plan_is_not_executable() -> Result<()> {
    let orchestrator = SequentialTaskOrchestrator::new(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("planner", "model");
    session.append_control(ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect implementation".to_owned(),
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Proposed,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step_1")?,
            title: "proposed".to_owned(),
            display_name: None,
            detail: None,
            role: crate::AgentRole::Executor,
        }],
        reason: None,
    }))?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let result = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "inspect implementation".to_owned(),
            },
            options(),
            options(),
            options(),
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await;

    assert!(result.is_err());
    Ok(())
}

#[test]
fn task_status_mapping_helpers_cover_terminal_edges() -> Result<()> {
    let step_id = TaskStepId::new("step_1")?;
    let output = |outcome| StepRunOutput {
        final_text: String::new(),
        outcome,
    };
    let recovered_output = |outcome| StepRunOutput {
        final_text: "recovered".to_owned(),
        outcome,
    };

    assert_eq!(
        step_status_from_outcome(&output(crate::AgentRunOutcome {
            terminal_reason: crate::AgentRunTerminalReason::MaxTurns,
            ..crate::AgentRunOutcome::default()
        })),
        TaskStepStatus::Interrupted
    );
    assert_eq!(
        step_status_from_outcome(&output(crate::AgentRunOutcome {
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::Internal,
                message: "boom".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        })),
        TaskStepStatus::Failed
    );
    assert_eq!(
        step_status_from_outcome(&recovered_output(crate::AgentRunOutcome {
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::InvalidInput,
                message: "bad path".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        })),
        TaskStepStatus::Completed
    );
    assert_eq!(
        step_status_from_outcome(&recovered_output(crate::AgentRunOutcome {
            approval_denials: 1,
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::ApprovalDenied,
                message: "denied".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        })),
        TaskStepStatus::Blocked
    );
    assert_eq!(
        step_status_from_outcome(&output(crate::AgentRunOutcome {
            approval_denials: 1,
            ..crate::AgentRunOutcome::default()
        })),
        TaskStepStatus::Blocked
    );
    assert_eq!(
        step_status_from_outcome(&output(crate::AgentRunOutcome {
            interrupted_tool_calls: vec!["call-1".to_owned()],
            ..crate::AgentRunOutcome::default()
        })),
        TaskStepStatus::Interrupted
    );

    assert_eq!(
        task_status_from_step_status(TaskStepStatus::Completed),
        TaskRunStatus::Completed
    );
    assert_eq!(
        task_status_from_step_status(TaskStepStatus::Failed),
        TaskRunStatus::Failed
    );
    assert_eq!(
        task_status_from_step_status(TaskStepStatus::Cancelled),
        TaskRunStatus::Cancelled
    );
    assert_eq!(
        task_status_from_step_status(TaskStepStatus::Running),
        TaskRunStatus::Paused
    );

    assert_eq!(
        step_terminal_reason(&step_id, TaskStepStatus::Failed),
        "step step_1 failed"
    );
    assert_eq!(
        step_terminal_reason(&step_id, TaskStepStatus::Blocked),
        "step step_1 blocked"
    );
    assert_eq!(
        step_terminal_reason(&step_id, TaskStepStatus::Cancelled),
        "step step_1 cancelled"
    );
    assert_eq!(
        step_terminal_reason(&step_id, TaskStepStatus::Pending),
        "step step_1 stopped"
    );

    assert_eq!(
        child_status_from_output(&output(crate::AgentRunOutcome {
            terminal_reason: crate::AgentRunTerminalReason::MaxTurns,
            ..crate::AgentRunOutcome::default()
        })),
        TaskChildSessionStatus::Interrupted
    );
    assert_eq!(
        child_status_from_output(&output(crate::AgentRunOutcome {
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::Internal,
                message: "boom".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        })),
        TaskChildSessionStatus::Failed
    );
    assert_eq!(
        child_status_from_output(&recovered_output(crate::AgentRunOutcome {
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::InvalidInput,
                message: "bad path".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        })),
        TaskChildSessionStatus::Completed
    );
    assert_eq!(
        child_status_from_output(&output(crate::AgentRunOutcome::default())),
        TaskChildSessionStatus::Completed
    );

    let route = route_id_for_call(
        &TaskId::new("task_1")?,
        &TaskStepId::new("step_1")?,
        "call-1",
    )?;
    assert!(route.as_str().starts_with("route_"));
    Ok(())
}

#[test]
fn task_step_readiness_marks_changed_files_unverified() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: Some("write note".to_owned()),
        role: crate::AgentRole::Executor,
    };
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
    };
    let session = Session::new("deepseek", "deepseek-v4-flash");
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "edited\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert_eq!(
        readiness.evaluation.visible_state,
        VisibleCompletionState::CompletedUnverified
    );
    Ok(())
}

#[test]
fn task_step_readiness_uses_recorded_check_specs_and_workspace_snapshot() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: Some("write note".to_owned()),
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "edited\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let candidate = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        source_event_id: "event-discovery".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    };
    let trusted = candidate.promote(
        "cargo-test",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            trusted,
            "event-discovery",
        ),
    ))?;
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(readiness.workspace_snapshot_id.is_some());
    assert!(readiness.evaluation.required_actions.iter().any(|action| {
        matches!(
            action,
            crate::RequiredAction::RunCheck { check_spec_id } if check_spec_id == "cargo-test"
        )
    }));
    Ok(())
}

#[test]
fn task_step_run_check_action_executes_configured_check_and_passes() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: Some("write note".to_owned()),
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let note_path = workspace.join("note.txt");
    std::fs::write(&note_path, "old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let recorder = session
        .mutation_event_recorder()
        .expect("store-backed session should create mutation recorder");
    write_file_with_mutation(
        Some(&recorder),
        &workspace,
        "tool-call-1",
        "note.txt",
        &note_path,
        b"new\n",
    )?;

    let mut options = options();
    options.workspace_root = workspace;
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "rustc".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "rustc-version",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            trusted,
            "event-config",
        ),
    ))?;
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
    };

    let missing = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;
    assert_eq!(
        missing.evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    let mut handler = RecordingEventHandler::default();
    assert!(run_task_step_verification_checks(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &missing,
    )?);

    let passed = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        passed.evaluation.verification_verdict,
        VerificationVerdict::Passed
    );
    assert_eq!(
        passed.evaluation.visible_state,
        VisibleCompletionState::Verified
    );
    assert!(session.verification_state_projection().receipts.len() == 1);
    Ok(())
}

#[test]
fn task_step_run_check_action_covers_empty_missing_and_failed_checks() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "verify a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "verify".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("note.txt"), "new\n")?;
    let mut options = options();
    options.workspace_root = workspace.clone();
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let mut handler = RecordingEventHandler::default();
    let no_action = crate::ReadinessEvaluatedEntry {
        scope: EvidenceScope::Step("task_1:step_1".to_owned()),
        evaluation: crate::ReadinessEvaluation {
            run_status: crate::RunStatus::Completed,
            verification_verdict: VerificationVerdict::NotApplicable,
            visible_state: VisibleCompletionState::Completed,
            reasons: Vec::new(),
            required_actions: Vec::new(),
        },
        policy_hash: None,
        workspace_snapshot_id: None,
    };
    assert!(!run_task_step_verification_checks(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &no_action,
    )?);

    let trust_only = crate::ReadinessEvaluatedEntry {
        evaluation: crate::ReadinessEvaluation {
            required_actions: vec![crate::RequiredAction::TrustWorkspace],
            ..no_action.evaluation.clone()
        },
        ..no_action.clone()
    };
    assert!(!run_task_step_verification_checks(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &trust_only,
    )?);

    let missing_spec = crate::ReadinessEvaluatedEntry {
        evaluation: crate::ReadinessEvaluation {
            required_actions: vec![crate::RequiredAction::RunCheck {
                check_spec_id: "missing-check".to_owned(),
            }],
            ..no_action.evaluation.clone()
        },
        ..no_action.clone()
    };
    let error = run_task_step_verification_checks(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &missing_spec,
    )
    .expect_err("missing trusted check should fail closed");
    assert!(
        error
            .to_string()
            .contains("missing trusted verification check spec")
    );

    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "false".to_owned(),
            args: Vec::new(),
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "always-fails",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            trusted,
            "event-config",
        ),
    ))?;
    let mut policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = session
        .verification_state_projection()
        .check_specs_for_scopes(&[EvidenceScope::Task("task_1".to_owned())])
        .into_iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect();
    policy.completion_criteria = crate::CompletionCriteria::AllRequiredChecks;
    let policy_entry = crate::VerificationPolicyChangedEntry::new(
        EvidenceScope::Task("task_1".to_owned()),
        policy,
        "event-policy",
    )?;
    let expected_policy_hash = policy_entry.policy_hash.clone();
    session.append_control(ControlEntry::VerificationPolicyChanged(policy_entry))?;
    let failed_check = crate::ReadinessEvaluatedEntry {
        evaluation: crate::ReadinessEvaluation {
            required_actions: vec![crate::RequiredAction::RunCheck {
                check_spec_id: "always-fails".to_owned(),
            }],
            ..no_action.evaluation.clone()
        },
        ..no_action.clone()
    };
    assert!(run_task_step_verification_checks(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &failed_check,
    )?);
    let receipts = session.verification_state_projection().receipts;
    assert!(
        receipts
            .values()
            .any(|entry| entry.receipt.check_status == crate::ReceiptStatus::Failed)
    );
    assert!(receipts.values().any(|entry| {
        entry.receipt.receipt.policy_hash.as_deref() == Some(expected_policy_hash.as_str())
    }));
    Ok(())
}

#[test]
fn task_step_status_blocks_when_readiness_requires_action() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "edited\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let session = Session::new("deepseek", "deepseek-v4-flash");
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        step_status_after_readiness(TaskStepStatus::Completed, &readiness),
        TaskStepStatus::Blocked
    );
    assert!(
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| matches!(action, crate::RequiredAction::ProvideVerificationConfig))
    );
    Ok(())
}

#[test]
fn task_step_readiness_records_recovered_tool_error_reason() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "verify".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "verify".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "unchanged\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let session = Session::new("deepseek", "deepseek-v4-flash");
    let output = StepRunOutput {
        final_text: "recovered".to_owned(),
        outcome: crate::AgentRunOutcome {
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::InvalidInput,
                message: "bad path".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        },
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert!(readiness.evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            crate::ReadinessReason::RecoveredToolError { event_id }
                if event_id.starts_with("task-step-recovered-tool-error:task_1:step_1:")
        )
    }));
    Ok(())
}

#[test]
fn task_step_verification_config_does_not_block_read_only_step() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect implementation".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "inspect".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::SubagentRead,
    };
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "unchanged\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let current_task_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand::shell("cargo test"),
        source_event_id: "event-current".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "cargo-test",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-current".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            current_task_check,
            "event-current",
        ),
    ))?;
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome::default(),
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::NotApplicable
    );
    assert!(readiness.evaluation.required_actions.is_empty());
    assert_eq!(
        step_status_after_readiness(TaskStepStatus::Completed, &readiness),
        TaskStepStatus::Completed
    );
    Ok(())
}

#[test]
fn task_step_default_policy_uses_only_current_task_scope() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "edited\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let other_task_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand::shell("npm test"),
        source_event_id: "event-other".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "cargo-test",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-other".to_owned(),
        },
    )?;
    let current_task_check = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand::shell("cargo test"),
        source_event_id: "event-current".to_owned(),
        workspace_trust_snapshot_id: "trust-1".to_owned(),
    }
    .promote(
        "cargo-test",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-current".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_2".to_owned()),
            other_task_check,
            "event-other",
        ),
    ))?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            current_task_check,
            "event-current",
        ),
    ))?;
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert!(
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| matches!(
                action,
                crate::RequiredAction::RunCheck { check_spec_id } if check_spec_id == "cargo-test"
            ))
    );
    let policy = session
        .verification_state_projection()
        .check_specs_for_scopes(&[EvidenceScope::Task("task_1".to_owned())])
        .into_iter()
        .map(|entry| entry.trusted_check.check_spec.command.command.clone())
        .collect::<Vec<_>>();
    assert_eq!(policy, vec!["cargo test".to_owned()]);
    Ok(())
}

#[test]
fn task_step_readiness_uses_projected_workspace_trust() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "verify".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "verify".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), "edited\n")?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let workspace_id = crate::stable_workspace_id(temp.path())?;
    session.append_control(ControlEntry::WorkspaceTrustDecision(
        WorkspaceTrustDecisionEntry {
            workspace_id,
            workspace_trust_snapshot_id: "trust-1".to_owned(),
            trust: WorkspaceTrust::Restricted,
            decided_by_event_id: Some("event-trust".to_owned()),
            reason: Some("test restricted".to_owned()),
        },
    ))?;
    let mut policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    policy.workspace_trust_requirement = crate::WorkspaceTrustRequirement::Trusted;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            policy,
            "event-policy",
        )?,
    ))?;
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome::default(),
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| matches!(action, crate::RequiredAction::TrustWorkspace))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn task_step_readiness_carries_unknown_dirty_snapshot_evidence() -> Result<()> {
    use std::os::unix::fs::symlink;

    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let temp = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("secret.txt"), "secret")?;
    symlink(outside.path().join("secret.txt"), temp.path().join("leak"))?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let session = Session::new("deepseek", "deepseek-v4-flash");
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["leak".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
    };

    let readiness = task_step_readiness(
        &session,
        &request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;

    assert_eq!(
        readiness.evaluation.verification_verdict,
        VerificationVerdict::Inconclusive
    );
    assert!(readiness.evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            crate::ReadinessReason::WorkspaceUnknownDirty {
                event_id: Some(event_id)
            } if event_id == "readiness-snapshot:task_1:step_1"
        )
    }));
    Ok(())
}

#[test]
fn durable_workspace_mutation_evidence_replays_stored_events_and_skips_legacy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    let legacy_entry = SessionLogEntry::User(ModelMessage::user("legacy"));
    std::fs::write(
        &log_path,
        format!("{}\n", serde_json::to_string(&legacy_entry)?),
    )?;
    let store = JsonlSessionStore::new(&log_path)?;
    let committed = store.append_event(
        DurableEventType::MutationCommitted,
        EventClass::Critical,
        json!({
            "operation_id": "op-commit",
            "workspace_id": "workspace-1",
            "observed_after_hash": "sha256:new",
            "workspace_revision": 2,
            "workspace_snapshot_id": "snapshot-2",
            "committed_subject": {
                "file": {
                    "path": "note.txt",
                    "file_type": "file"
                }
            }
        }),
    )?;
    let reconciled = store.append_event(
        DurableEventType::MutationReconciled,
        EventClass::Critical,
        json!({
            "operation_id": "op-reconcile",
            "observed_state": "unknown",
            "resolution": "mark_unknown_dirty",
            "workspace_revision": 3,
            "workspace_snapshot_id": "snapshot-unknown"
        }),
    )?;
    let detected = store.append_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        json!({ "source": "bash" }),
    )?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let evidence = durable_workspace_mutation_evidence(
        &session,
        &crate::VerificationScope::all_tracked("scope-main"),
    );

    assert_eq!(evidence.len(), 3);
    assert_eq!(evidence[0].event_id, committed.event_id);
    assert_eq!(
        evidence[0].to_workspace_snapshot_id.as_deref(),
        Some("snapshot-2")
    );
    assert!(!evidence[0].unknown_dirty);
    assert_eq!(evidence[1].event_id, reconciled.event_id);
    assert_eq!(evidence[1].tool_effect, ToolEffect::Unknown);
    assert!(evidence[1].unknown_dirty);
    assert_eq!(evidence[2].event_id, detected.event_id);
    assert_eq!(evidence[2].source_event_type, "workspace_mutation_detected");
    assert!(evidence[2].unknown_dirty);
    Ok(())
}

#[test]
fn durable_workspace_mutation_evidence_fails_closed_to_empty_on_unreadable_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    std::fs::write(&log_path, "{not-json}\n")?;
    let store = JsonlSessionStore::new(&log_path)?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    assert!(
        durable_workspace_mutation_evidence(
            &session,
            &crate::VerificationScope::all_tracked("scope-main"),
        )
        .is_empty()
    );
    Ok(())
}

#[test]
fn run_status_from_step_status_covers_running_cancelled_and_interrupted() {
    assert_eq!(
        run_status_from_step_status(TaskStepStatus::Pending),
        crate::RunStatus::Running
    );
    assert_eq!(
        run_status_from_step_status(TaskStepStatus::Running),
        crate::RunStatus::Running
    );
    assert_eq!(
        run_status_from_step_status(TaskStepStatus::Cancelled),
        crate::RunStatus::Cancelled
    );
    assert_eq!(
        run_status_from_step_status(TaskStepStatus::Interrupted),
        crate::RunStatus::Interrupted
    );
}

fn boxed_agent<P>(provider: P, registry: ToolRegistry) -> Agent<Box<dyn Provider>>
where
    P: Provider + 'static,
{
    Agent::new(Box::new(provider), registry)
}

fn seed_two_step_task(
    session: &mut Session,
    status: TaskRunStatus,
    first_step_completed: bool,
) -> Result<()> {
    session.append_control(ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect implementation".to_owned(),
        status,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![
            TaskStepSpec {
                step_id: TaskStepId::new("step_1")?,
                title: "already done".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
            },
            TaskStepSpec {
                step_id: TaskStepId::new("step_2")?,
                title: "remaining".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
            },
        ],
        reason: None,
    }))?;
    if first_step_completed {
        session.append_control(ControlEntry::TaskStep(crate::TaskStepEntry {
            task_id: TaskId::new("task_1")?,
            plan_version: 1,
            step_id: TaskStepId::new("step_1")?,
            role: crate::AgentRole::Executor,
            status: TaskStepStatus::Completed,
            title: Some("already done".to_owned()),
            summary: Some("done".to_owned()),
            reason: None,
        }))?;
    }
    Ok(())
}

fn seed_single_step_task(session: &mut Session, role: crate::AgentRole) -> Result<()> {
    session.append_control(ControlEntry::TaskRun(crate::TaskRunEntry {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect implementation".to_owned(),
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: TaskId::new("task_1")?,
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step_1")?,
            title: "single step".to_owned(),
            display_name: None,
            detail: Some("detail".to_owned()),
            role,
        }],
        reason: None,
    }))?;
    Ok(())
}

fn options() -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: std::env::current_dir().expect("test cwd should resolve"),
        max_turns: Some(4),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: crate::CompactionConfig::default(),
    }
}

fn capabilities() -> ProviderCapabilities {
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
