use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};

use crate::{
    Agent, AgentRunOptions, AutoApproveHandler, CompletionRequest, ControlEntry, InteractionMode,
    JsonlSessionStore, MemoryConfig, MessageRole, PermissionConfig, Provider, ProviderCapabilities,
    ProviderChunk, ReasoningEffort, SequentialTaskOrchestrator, SequentialTaskRequest, Session,
    SessionLogEntry, SessionRef, TASK_PLAN_UPDATE_TOOL_NAME, TaskChildSessionStatus, TaskId,
    TaskPlanEntry, TaskPlanStatus, TaskRouteStatus, TaskRunStatus, TaskStepId, TaskStepSpec,
    TaskStepStatus, Tool, ToolAccess, ToolApproval, ToolCall, ToolCategory, ToolContext,
    ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
};

use super::{
    StepRunOutput, child_status_from_outcome, route_id_for_call, step_status_from_outcome,
    step_terminal_reason, task_status_from_step_status,
};

struct PlannerProvider;
struct NoPlanProvider;
struct FailingProvider;
struct ToolCallingProvider;
struct ApprovalRequiredTool;
struct DenyApprovalHandler;

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
            .any(|message| matches!(message.role, MessageRole::Tool));
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
    let mut handler = crate::event::NoopEventHandler;
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
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.plan_version, 1);
    assert_eq!(output.steps.len(), 1);
    assert_eq!(output.steps[0].step_id, TaskStepId::new("step_2")?);
    let requests = executor_requests
        .lock()
        .expect("executor request lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("Step: step_2"))
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
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Failed);
    assert_eq!(output.steps[0].status, TaskStepStatus::Failed);
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
async fn child_step_defensive_parent_role_fallback_uses_executor_agent() -> Result<()> {
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
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "delegate through fallback".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "fallback".to_owned(),
        detail: None,
        role: crate::AgentRole::Executor,
    };
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .run_child_step(
            &mut session,
            &request,
            1,
            &step,
            options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.final_text, "step complete");
    assert_eq!(
        executor_requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .len(),
        1
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
        child_status_from_outcome(&crate::AgentRunOutcome {
            terminal_reason: crate::AgentRunTerminalReason::MaxTurns,
            ..crate::AgentRunOutcome::default()
        }),
        TaskChildSessionStatus::Interrupted
    );
    assert_eq!(
        child_status_from_outcome(&crate::AgentRunOutcome {
            tool_errors: vec![crate::ToolError {
                kind: crate::ToolErrorKind::Internal,
                message: "boom".to_owned(),
                retryable: false,
                details: Value::Null,
            }],
            ..crate::AgentRunOutcome::default()
        }),
        TaskChildSessionStatus::Failed
    );
    assert_eq!(
        child_status_from_outcome(&crate::AgentRunOutcome::default()),
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
                detail: None,
                role: crate::AgentRole::Executor,
            },
            TaskStepSpec {
                step_id: TaskStepId::new("step_2")?,
                title: "remaining".to_owned(),
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
            detail: Some("detail".to_owned()),
            role,
        }],
        reason: None,
    }))?;
    Ok(())
}

fn options() -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: std::env::temp_dir(),
        max_turns: Some(4),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: crate::CompactionConfig::default(),
    }
}

fn capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: false,
        reports_cache_tokens: false,
        supports_reasoning_stream: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: false,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: false,
        supports_infill_completion: false,
        supports_system_fingerprint: false,
        tool_name_max_chars: 64,
    }
}
