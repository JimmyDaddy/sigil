use std::{
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};

use crate::{
    Agent, AgentRunInput, AgentRunOptions, AutoApproveHandler, CandidateCheck, CheckCommand,
    CheckDiscoverySource, CheckPromotion, CheckSpec, CheckSpecRecordedEntry, CheckpointRestored,
    CompletionRequest, ControlEntry, DEFAULT_TASK_VERIFICATION_SCOPE_HASH, DurableEventType,
    EventClass, EvidenceScope, ExecutionBackend, ExecutionBackendCapabilities,
    ExecutionBackendKind, ExecutionFuture, ExecutionMutationProfile, ExecutionReceipt,
    ExecutionRequest, FileType, InteractionMode, JsonlSessionStore, MemoryConfig, MessageRole,
    ModelMessage, MutationEventRecorder, MutationPrepared, MutationSubject, MutationSyncClass,
    PermissionConfig, Provider, ProviderCapabilities, ProviderChunk, ReasoningEffort,
    ReasoningStreamSupport, RunEvent, SequentialTaskOrchestrator, SequentialTaskRequest, Session,
    SessionLogEntry, SessionRef, SnapshotCoverage, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildSessionStatus, TaskId, TaskIsolationMode, TaskPlanEntry, TaskPlanStatus, TaskRunEntry,
    TaskRunStatus, TaskStepId, TaskStepMode, TaskStepSpec, TaskStepStatus,
    TaskVerificationRerunRequest, TerminalTaskEntry, TerminalTaskHandle, TerminalTaskId,
    TerminalTaskStatus, Tool, ToolAccess, ToolApproval, ToolCall, ToolCategory, ToolContext,
    ToolEffect, ToolExecutionEntry, ToolExecutionStatus, ToolPreviewCapability, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSpec, TrustedCheckSpec, VerificationAutoRunPolicy,
    VerificationVerdict, VisibleCompletionState, WorkspaceKnowledge, WorkspaceMutationDetected,
    WorkspaceMutationDetectionReason, WorkspaceTrust, WorkspaceTrustDecisionEntry,
    WriteIsolationMode, WriteLeaseAcquired, WriteLeaseId, WriteLeaseReleaseStatus, WriteLeaseScope,
    stable_workspace_id, write_file_with_mutation,
};

use super::{
    StepRunOutput, TaskChildSessionRunOutput, TaskChildSessionRunRequest, TaskChildSessionRunner,
    child_status_from_output, decode_changeset_only_child_output,
    durable_workspace_mutation_evidence, latest_relevant_successful_verification_sequence,
    planner_prompt, relevant_verification_receipts, rerun_task_verification_check,
    route_id_for_call, run_status_from_step_status, run_task_step_verification_checks,
    step_status_after_readiness, step_status_from_outcome, step_terminal_reason,
    task_status_from_step_status, task_step_auto_run_policy, task_step_default_policy,
    task_step_readiness,
};

struct PlannerProvider;
struct NoPlanProvider;
struct FailingProvider;
struct ToolCallingProvider;
struct MutatingToolProvider;
struct RecoveringToolErrorProvider;
struct RecoverableErrorTool;
struct NamedFixtureTool {
    name: &'static str,
    category: ToolCategory,
    access: ToolAccess,
    network_effect: Option<crate::NetworkEffect>,
}
struct MutatingTool;
struct ApprovalRequiredTool;
struct DenyApprovalHandler;
#[derive(Clone)]
struct StaticChangesetChildRunner {
    final_text: String,
    outcome: crate::AgentRunOutcome,
    mutate_parent_file: Option<PathBuf>,
}
#[derive(Debug, Default)]
struct FakeTaskExecutionBackend;
#[derive(Default)]
struct RecordingEventHandler {
    events: Vec<RunEvent>,
}

impl ExecutionBackend for FakeTaskExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        Box::pin(async move {
            if request.cwd.ends_with("missing-cwd") {
                return Err(anyhow::anyhow!("fake spawn failed for {}", request.program));
            }
            let failed = request.program == "false";
            Ok(ExecutionReceipt {
                backend: ExecutionBackendKind::Local,
                capabilities: ExecutionBackendCapabilities::default(),
                network: Default::default(),
                resources: Default::default(),
                environment_policy: request.environment_policy,
                exit_code: if failed { Some(1) } else { Some(0) },
                stdout: format!("fake backend executed {}\n", request.program).into_bytes(),
                stderr: if failed {
                    b"fake verification failure\n".to_vec()
                } else {
                    Vec::new()
                },
                output: Default::default(),
                timed_out: false,
            })
        })
    }
}

fn run_task_step_verification_checks_with_fake_backend<H>(
    session: &mut Session,
    handler: &mut H,
    request: &SequentialTaskRequest,
    step: &TaskStepSpec,
    options: &AgentRunOptions,
    readiness: &crate::ReadinessEvaluatedEntry,
) -> Result<bool>
where
    H: crate::EventHandler + Send,
{
    let backend = FakeTaskExecutionBackend;
    futures::executor::block_on(run_task_step_verification_checks(
        session,
        handler,
        Some(&backend),
        request,
        step,
        options,
        readiness,
    ))
}

struct TaskVerificationRerunFixture {
    _temp: tempfile::TempDir,
    workspace: PathBuf,
    session: Session,
    request: TaskVerificationRerunRequest,
}

fn task_verification_rerun_fixture() -> Result<TaskVerificationRerunFixture> {
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    let task_request = SequentialTaskRequest {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: step_id.clone(),
        title: "edit".to_owned(),
        display_name: None,
        detail: Some("write note".to_owned()),
        role: crate::AgentRole::Executor,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workspace = std::fs::canonicalize(workspace)?;
    std::fs::write(workspace.join("note.txt"), "current\n")?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
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
    let check_spec = trusted.check_spec.clone();
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task(task_id.as_str().to_owned()),
            trusted,
            "event-config",
        ),
    ))?;
    let mut policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = vec![check_spec.clone()];
    policy.completion_criteria = crate::CompletionCriteria::AllRequiredChecks;
    policy.timeout_ms = Some(60_000);
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(
            EvidenceScope::Task(task_id.as_str().to_owned()),
            policy,
            "event-policy",
        )?,
    ))?;
    let mut options = options();
    options.workspace_root = workspace.clone();
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },
        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
    };
    let readiness = task_step_readiness(
        &session,
        &task_request,
        &step,
        TaskStepStatus::Completed,
        &output,
        &options,
    )?;
    session.append_control(ControlEntry::ReadinessEvaluated(readiness.clone()))?;
    let request = TaskVerificationRerunRequest {
        task_id,
        step_id,
        check_spec_id: check_spec.check_spec_id,
        check_spec_hash: check_spec.check_spec_hash,
        policy_hash: readiness
            .policy_hash
            .expect("task readiness should bind the policy"),
        workspace_snapshot_id: readiness
            .workspace_snapshot_id
            .expect("changed task readiness should bind the workspace snapshot"),
    };
    Ok(TaskVerificationRerunFixture {
        _temp: temp,
        workspace,
        session,
        request,
    })
}

impl crate::EventHandler for RecordingEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        self.events.push(event);
        Ok(())
    }
}

fn test_orchestrator(
    planner: Agent<Box<dyn Provider>>,
    executor: Agent<Box<dyn Provider>>,
    subagent_read: Agent<Box<dyn Provider>>,
    subagent_write: Agent<Box<dyn Provider>>,
) -> SequentialTaskOrchestrator<super::TestAgentTaskChildSessionRunner> {
    SequentialTaskOrchestrator::new_with_child_runner(
        planner,
        executor,
        super::TestAgentTaskChildSessionRunner::new(subagent_read, subagent_write),
    )
}

#[async_trait]
impl TaskChildSessionRunner for StaticChangesetChildRunner {
    async fn run_child_session<H, A>(
        &self,
        _parent_session: &mut Session,
        request: TaskChildSessionRunRequest,
        _handler: &mut H,
        _approval_handler: &mut A,
    ) -> Result<TaskChildSessionRunOutput>
    where
        H: crate::EventHandler + Send,
        A: crate::ApprovalHandler + Send,
    {
        if let Some(path) = &self.mutate_parent_file {
            std::fs::write(request.options.workspace_root.join(path), b"mutated")?;
        }
        let changeset_proposal =
            if request.step.effective_isolation() == crate::TaskIsolationMode::ChangesetOnly {
                Some(decode_changeset_only_child_output(&self.final_text)?)
            } else {
                None
            };
        let changeset_only_after_snapshot_id =
            if let Some(base_snapshot_id) = request.changeset_only_base_snapshot_id.as_deref() {
                Some(
                    crate::validate_changeset_only_parent_snapshot_unchanged_for_task(
                        _parent_session,
                        &request.task,
                        request.plan_version,
                        &request.step,
                        &request.options,
                        base_snapshot_id,
                    )?,
                )
            } else {
                None
            };
        Ok(TaskChildSessionRunOutput {
            final_text: self.final_text.clone(),
            outcome: self.outcome.clone(),
            changeset_proposal,
            changeset_only_after_snapshot_id,
        })
    }
}

#[test]
fn planner_prompt_explains_subagent_delegation_without_direct_task_tool() {
    let prompt = planner_prompt("review implementation");

    assert!(prompt.contains("Do not call a task or subagent tool"));
    assert!(prompt.contains("role executor for ordinary main-session reads and edits"));
    assert!(prompt.contains("role subagent_read"));
    assert!(prompt.contains("role subagent_write only for delegated changeset-only"));
    assert!(prompt.contains("do not pair subagent_write with sequential_workspace_write"));
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
        super::TestAgentTaskChildSessionRunner::new(
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
        Ok(ToolResult::error(
            call_id,
            "recoverable_error",
            crate::ToolErrorKind::InvalidInput,
            "bad path",
        ))
    }
}

#[async_trait]
impl Tool for NamedFixtureTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name.to_owned(),
            description: "fixture tool".to_owned(),
            input_schema: json!({"type":"object","properties":{}}),
            category: self.category,
            access: self.access,
            network_effect: self.network_effect,
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
            self.name,
            "ok",
            ToolResultMeta::default(),
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
            network_effect: None,
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
            network_effect: None,
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
    let orchestrator = test_orchestrator(
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
async fn sequential_task_orchestrator_continues_dependent_steps_until_completed() -> Result<()> {
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = test_orchestrator(
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
    seed_task_with_steps(
        &mut session,
        TaskRunStatus::Paused,
        vec![
            read_executor_step("verify_current", "verify current", Vec::new())?,
            read_executor_step(
                "fix_typo",
                "fix typo",
                vec![TaskStepId::new("verify_current")?],
            )?,
            read_executor_step(
                "verify_fix",
                "verify fix",
                vec![TaskStepId::new("fix_typo")?],
            )?,
        ],
    )?;
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "fix typo".to_owned(),
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
    assert_eq!(output.steps.len(), 3);
    assert!(
        output
            .steps
            .iter()
            .all(|step| step.status == TaskStepStatus::Completed)
    );
    assert_eq!(
        executor_requests
            .lock()
            .expect("executor request lock should not be poisoned")
            .len(),
        3
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Completed
                    && run.reason.as_deref() == Some("completed plan v1")
        )
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
    append_trusted_only_policy_for_task(&mut session, "task_1")?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = test_orchestrator(
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
    )
    .with_execution_backend(Arc::new(FakeTaskExecutionBackend));
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
async fn sequential_task_orchestrator_completes_mutating_step_without_verification_config()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("planner", "model").with_store(store);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = test_orchestrator(
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

    assert_eq!(output.status, TaskRunStatus::Completed);
    assert_eq!(output.steps[0].status, TaskStepStatus::Completed);
    assert_eq!(
        output.steps[0].verification_verdict,
        VerificationVerdict::Missing
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness))
                if readiness.evaluation.run_status == crate::RunStatus::Completed
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
    let orchestrator = test_orchestrator(
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
    let lease_projection = session.write_isolation_projection();
    assert_eq!(lease_projection.leases.len(), 1);
    let lease_state = lease_projection
        .leases
        .values()
        .next()
        .expect("write lease state");
    assert!(lease_state.acquired.is_some());
    assert!(matches!(
        lease_state.released.as_ref(),
        Some(release) if release.status == WriteLeaseReleaseStatus::Completed
    ));
    Ok(())
}

#[tokio::test]
async fn continue_run_pauses_when_active_workspace_write_lease_defers_ready_step() -> Result<()> {
    let orchestrator = test_orchestrator(
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
    let options = options();
    session.append_control(ControlEntry::WriteLeaseAcquired(WriteLeaseAcquired {
        lease_id: WriteLeaseId::new("lease-existing")?,
        workspace_id: stable_workspace_id(&options.workspace_root)?,
        owner_agent_id: "other-agent".to_owned(),
        isolation_mode: WriteIsolationMode::SharedWorkspaceExclusive,
        scope: WriteLeaseScope::Workspace,
    }))?;
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
            options.clone(),
            options.clone(),
            options,
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Paused);
    assert!(output.steps.is_empty());
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Paused
                    && run.reason.as_deref().is_some_and(|reason| {
                        reason.contains("active_write_lease")
                    })
        )
    }));
    Ok(())
}

#[tokio::test]
async fn task_write_isolation_ready_queue_defers_write_until_read_dependencies_complete()
-> Result<()> {
    let read_requests = Arc::new(Mutex::new(Vec::new()));
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = test_orchestrator(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&executor_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&read_requests),
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
    seed_task_with_steps(
        &mut session,
        TaskRunStatus::Paused,
        vec![
            TaskStepSpec {
                step_id: TaskStepId::new("read_a")?,
                title: "read A".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("read_b")?,
                title: "read B".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("write")?,
                title: "write".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
                depends_on: vec![TaskStepId::new("read_a")?, TaskStepId::new("read_b")?],
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            },
        ],
    )?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "read before write".to_owned(),
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
    assert_eq!(output.steps.len(), 3);
    assert_eq!(
        output
            .steps
            .iter()
            .map(|step| step.step_id.as_str())
            .collect::<Vec<_>>(),
        vec!["read_a", "read_b", "write"]
    );
    assert_eq!(
        read_requests
            .lock()
            .expect("read requests should not be poisoned")
            .len(),
        2
    );
    assert_eq!(
        executor_requests
            .lock()
            .expect("executor requests should not be poisoned")
            .len(),
        1
    );
    let entries = session.entries();
    let read_a_completed = task_step_entry_index(entries, "read_a", TaskStepStatus::Completed)
        .expect("read_a should complete before write lease");
    let read_b_completed = task_step_entry_index(entries, "read_b", TaskStepStatus::Completed)
        .expect("read_b should complete before write lease");
    let write_lease_acquired = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::WriteLeaseAcquired(_))
            )
        })
        .expect("write step should acquire a lease");
    assert!(write_lease_acquired > read_a_completed);
    assert!(write_lease_acquired > read_b_completed);
    let lease_projection = session.write_isolation_projection();
    assert_eq!(lease_projection.leases.len(), 1);
    assert!(
        lease_projection
            .leases
            .values()
            .all(|lease| !lease.is_active())
    );
    Ok(())
}

#[tokio::test]
async fn task_write_isolation_active_lease_pauses_ready_queue_without_running_steps() -> Result<()>
{
    let read_requests = Arc::new(Mutex::new(Vec::new()));
    let executor_requests = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = test_orchestrator(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&executor_requests),
            },
            ToolRegistry::new(),
        ),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::clone(&read_requests),
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
    seed_task_with_steps(
        &mut session,
        TaskRunStatus::Paused,
        vec![
            TaskStepSpec {
                step_id: TaskStepId::new("read")?,
                title: "read".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentRead,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("write")?,
                title: "write".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            },
        ],
    )?;
    let run_options = options();
    session.append_control(ControlEntry::WriteLeaseAcquired(WriteLeaseAcquired {
        lease_id: WriteLeaseId::new("lease-existing")?,
        workspace_id: stable_workspace_id(&run_options.workspace_root)?,
        owner_agent_id: "other-agent".to_owned(),
        isolation_mode: WriteIsolationMode::SharedWorkspaceExclusive,
        scope: WriteLeaseScope::Workspace,
    }))?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "blocked by lease".to_owned(),
            },
            run_options.clone(),
            run_options.clone(),
            run_options,
            None,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Paused);
    assert!(output.steps.is_empty());
    assert_eq!(
        read_requests
            .lock()
            .expect("read requests should not be poisoned")
            .len(),
        0
    );
    assert_eq!(
        executor_requests
            .lock()
            .expect("executor requests should not be poisoned")
            .len(),
        0
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskRun(run))
                if run.status == TaskRunStatus::Paused
                    && run.reason.as_deref().is_some_and(|reason| {
                        reason.contains("active_write_lease")
                    })
        )
    }));
    Ok(())
}

#[tokio::test]
async fn task_write_isolation_cancels_dependents_after_failed_write() -> Result<()> {
    let orchestrator = test_orchestrator(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
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
    );
    let mut session = Session::new("planner", "model");
    seed_task_with_steps(
        &mut session,
        TaskRunStatus::Paused,
        vec![
            TaskStepSpec {
                step_id: TaskStepId::new("write")?,
                title: "write".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Write),
                isolation: Some(TaskIsolationMode::SequentialWorkspaceWrite),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("verify")?,
                title: "verify".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::SubagentRead,
                depends_on: vec![TaskStepId::new("write")?],
                mode: Some(TaskStepMode::Verify),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            },
        ],
    )?;
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = AutoApproveHandler;

    let output = orchestrator
        .continue_run(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "failed write cancels dependent".to_owned(),
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
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.step_id == TaskStepId::new("write").expect("valid step id")
                    && step.status == TaskStepStatus::Failed
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.step_id == TaskStepId::new("verify").expect("valid step id")
                    && step.status == TaskStepStatus::Cancelled
                    && step.reason.as_deref().is_some_and(|reason| {
                        reason.contains("dependency write ended with failed")
                    })
        )
    }));
    Ok(())
}

#[tokio::test]
async fn changeset_only_child_records_proposal_without_parent_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), b"old\n")?;
    let runner = StaticChangesetChildRunner {
        final_text: changeset_only_child_final_text("change-note"),
        outcome: crate::AgentRunOutcome::default(),
        mutate_parent_file: None,
    };
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        runner,
    );
    let mut session = Session::new("planner", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let options = options_for_workspace(temp.path());

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "propose note change".to_owned(),
            },
            changeset_only_step()?,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                "propose note change",
            )]),
            options.clone(),
            options,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Paused);
    assert_eq!(output.steps[0].status, TaskStepStatus::Blocked);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("note.txt"))?,
        "old\n"
    );
    assert!(session.entries().iter().all(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::WriteLeaseAcquired(_))
        )
    }));
    let proposed = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ChangeSetProposed(change_set)) => {
                Some(change_set)
            }
            _ => None,
        })
        .expect("changeset proposed");
    assert_eq!(proposed.id.as_str(), "change-note");
    let produced = session
        .write_isolation_projection()
        .isolated_changesets
        .get(&proposed.id)
        .cloned()
        .expect("isolated changeset produced");
    assert_eq!(produced.source_isolation, WriteIsolationMode::ChangesetOnly);
    assert!(produced.child_snapshot_id.is_none());
    assert!(!produced.base_snapshot_id.is_empty());
    assert!(
        produced
            .artifact_ref
            .as_deref()
            .is_some_and(|artifact_ref| artifact_ref.starts_with("inline:sha256:"))
    );
    assert_eq!(
        produced.touched_subjects,
        vec![MutationSubject::File {
            path: PathBuf::from("note.txt"),
            file_type: FileType::File,
        }]
    );
    let projection = session.write_isolation_projection();
    let review = projection
        .merge_reviews
        .values()
        .next()
        .expect("merge review requested");
    assert_eq!(
        review
            .requested
            .as_ref()
            .map(|request| &request.changeset_id),
        Some(&proposed.id)
    );
    Ok(())
}

#[tokio::test]
async fn changeset_only_child_registry_filters_unsafe_same_name_tools() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(NamedFixtureTool {
        name: "grep",
        category: ToolCategory::Search,
        access: ToolAccess::Read,
        network_effect: Some(crate::NetworkEffect::Read),
    }));
    registry.register(Arc::new(NamedFixtureTool {
        name: "read_file",
        category: ToolCategory::File,
        access: ToolAccess::Write,
        network_effect: None,
    }));
    registry.register(Arc::new(NamedFixtureTool {
        name: "ls",
        category: ToolCategory::Search,
        access: ToolAccess::Read,
        network_effect: None,
    }));

    let scoped = crate::changeset_only_child_tool_registry(&registry);

    assert!(scoped.spec_for("ls").is_some());
    assert!(scoped.spec_for("grep").is_none());
    assert!(scoped.spec_for("read_file").is_none());

    Ok(())
}

#[tokio::test]
async fn changeset_only_child_fails_when_parent_snapshot_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("note.txt"), b"old\n")?;
    let runner = StaticChangesetChildRunner {
        final_text: changeset_only_child_final_text("change-note"),
        outcome: crate::AgentRunOutcome::default(),
        mutate_parent_file: Some(PathBuf::from("note.txt")),
    };
    let orchestrator = SequentialTaskOrchestrator::new_with_child_runner(
        boxed_agent(PlannerProvider, ToolRegistry::new()),
        boxed_agent(
            CapturingExecutorProvider {
                requests: Arc::new(Mutex::new(Vec::new())),
            },
            ToolRegistry::new(),
        ),
        runner,
    );
    let mut session = Session::new("planner", "model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let options = options_for_workspace(temp.path());

    let output = orchestrator
        .run_direct_child_session(
            &mut session,
            SequentialTaskRequest {
                task_id: TaskId::new("task_1")?,
                parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                objective: "propose note change".to_owned(),
            },
            changeset_only_step()?,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                "propose note change",
            )]),
            options.clone(),
            options,
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.status, TaskRunStatus::Failed);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.status == TaskStepStatus::Failed
                    && step
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("changed parent workspace snapshot"))
        )
    }));
    assert!(session.entries().iter().all(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::IsolatedChangeSetProduced(_))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn continue_run_continues_after_recovered_tool_error() -> Result<()> {
    let mut executor_registry = ToolRegistry::new();
    executor_registry.register(Arc::new(RecoverableErrorTool));
    let orchestrator = test_orchestrator(
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
    seed_task_with_steps(
        &mut session,
        TaskRunStatus::Paused,
        vec![
            TaskStepSpec {
                step_id: TaskStepId::new("step_1")?,
                title: "recoverable read".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Planner,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            },
            TaskStepSpec {
                step_id: TaskStepId::new("step_2")?,
                title: "follow-up read".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Planner,
                depends_on: Vec::new(),
                mode: Some(TaskStepMode::Read),
                isolation: Some(TaskIsolationMode::SharedReadOnly),
            },
        ],
    )?;
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
    let orchestrator = test_orchestrator(
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
    let orchestrator = test_orchestrator(
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
    let orchestrator = test_orchestrator(
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
    let orchestrator = test_orchestrator(
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
            depends_on: Vec::new(),
            mode: None,
            isolation: None,
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
    let orchestrator = test_orchestrator(
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
    append_trusted_only_policy_for_task(&mut session, "task_skill")?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = test_orchestrator(
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
    )
    .with_execution_backend(Arc::new(FakeTaskExecutionBackend));
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
async fn direct_child_session_completes_mutating_write_without_verification_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("planner", "model").with_store(store);
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(MutatingTool));
    let orchestrator = test_orchestrator(
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
        VerificationVerdict::Missing
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(readiness))
                if readiness.evaluation.run_status == crate::RunStatus::Completed
                    && readiness
                        .evaluation
                        .required_actions
                        .contains(&crate::RequiredAction::ProvideVerificationConfig)
        )
    }));
    Ok(())
}

#[tokio::test]
async fn direct_child_session_rejects_non_subagent_roles() -> Result<()> {
    let orchestrator = test_orchestrator(
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
    let orchestrator = test_orchestrator(
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
    let orchestrator = test_orchestrator(
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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
async fn subagent_write_step_rejects_non_changeset_isolation_before_denied_route() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session-parent.jsonl"))?;
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ApprovalRequiredTool));
    let orchestrator = test_orchestrator(
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

    let error = orchestrator
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
        .await
        .expect_err("non-changeset subagent write plans must be rejected before routing approvals");

    assert!(error.to_string().contains("requires changeset_only"));
    assert!(session.entries().iter().all(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(_))
        )
    }));
    assert!(!temp.path().join("children/task_1").exists());
    Ok(())
}

#[tokio::test]
async fn subagent_write_step_rejects_non_changeset_isolation_before_approved_route() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ApprovalRequiredTool));
    let orchestrator = test_orchestrator(
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

    let error = orchestrator
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
        .await
        .expect_err("non-changeset subagent write plans must be rejected before routing approvals");

    assert!(error.to_string().contains("requires changeset_only"));
    assert!(session.entries().iter().all(|entry| {
        !matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskSubagentApprovalRoute(_))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn child_step_rejects_parent_role_in_child_runner() -> Result<()> {
    let orchestrator = test_orchestrator(
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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
    let orchestrator = test_orchestrator(
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
            depends_on: Vec::new(),
            mode: None,
            isolation: None,
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
    let orchestrator = test_orchestrator(
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
    let orchestrator = test_orchestrator(
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
    let orchestrator = test_orchestrator(
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
            depends_on: Vec::new(),
            mode: None,
            isolation: None,
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

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
    };
    let recovered_output = |outcome| StepRunOutput {
        final_text: "recovered".to_owned(),
        outcome,

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        task_status_from_step_status(TaskStepStatus::Superseded),
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
        step_terminal_reason(&step_id, TaskStepStatus::Superseded),
        "step step_1 superseded"
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
fn task_step_readiness_uses_durable_mutation_without_changed_files() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "run shell that edits a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: Some("write note through shell".to_owned()),
        role: crate::AgentRole::Executor,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("note.txt"), "old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    let recorder = MutationEventRecorder::new(store);
    let scope = crate::VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let scan = recorder.capture_workspace_scan(&workspace, &scope)?;
    std::fs::write(workspace.join("note.txt"), "new\n")?;
    recorder
        .record_workspace_mutation_if_changed(
            &scan,
            &workspace,
            "call-shell",
            "bash",
            ToolEffect::Unknown,
        )?
        .expect("changed workspace should produce durable mutation evidence");

    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            tool_call_ids: vec!["call-shell".to_owned()],
            ..crate::AgentRunOutcome::default()
        },

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
    };
    let mut options = options();
    options.workspace_root = workspace;

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
fn task_step_readiness_uses_post_task_mutation_from_prior_tool_call() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "cancel a terminal that already wrote a file".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_2")?,
        title: "cancel".to_owned(),
        display_name: None,
        detail: Some("cancel terminal".to_owned()),
        role: crate::AgentRole::Executor,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("note.txt"), "old\n")?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: request.task_id.clone(),
        parent_session_ref: request.parent_session_ref.clone(),
        objective: request.objective.clone(),
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    let recorder = MutationEventRecorder::new(store);
    let scope = crate::VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    let scan = recorder.capture_workspace_scan(&workspace, &scope)?;
    std::fs::write(workspace.join("note.txt"), "terminal wrote\n")?;
    recorder
        .record_workspace_mutation_if_changed(
            &scan,
            &workspace,
            "call-terminal-start",
            "terminal_start",
            ToolEffect::Unknown,
        )?
        .expect("terminal mutation should be recorded after task start");

    let output = StepRunOutput {
        final_text: "cancelled".to_owned(),
        outcome: crate::AgentRunOutcome {
            tool_call_ids: vec!["call-terminal-cancel".to_owned()],
            ..crate::AgentRunOutcome::default()
        },

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
    };
    let mut options = options();
    options.workspace_root = workspace;

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
            .contains(&crate::RequiredAction::ProvideVerificationConfig)
    );
    Ok(())
}

#[test]
fn task_step_readiness_treats_durable_mutation_replay_failure_as_unknown_dirty() -> Result<()> {
    let request = SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "finish after corrupt durable stream".to_owned(),
    };
    let step = TaskStepSpec {
        step_id: TaskStepId::new("step_1")?,
        title: "edit".to_owned(),
        display_name: None,
        detail: Some("durable replay failed".to_owned()),
        role: crate::AgentRole::Executor,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    std::fs::write(workspace.join("note.txt"), "unchanged\n")?;
    let log_path = temp.path().join("state/session.jsonl");
    std::fs::create_dir_all(log_path.parent().expect("state path should have parent"))?;
    std::fs::write(&log_path, "{not-json}\n")?;
    let store = JsonlSessionStore::new(&log_path)?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            tool_call_ids: vec!["call-shell".to_owned()],
            ..crate::AgentRunOutcome::default()
        },

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
    };
    let mut options = options();
    options.workspace_root = workspace;

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
    assert!(
        readiness
            .evaluation
            .required_actions
            .contains(&crate::RequiredAction::ResolveUnknownDirty)
    );
    assert!(readiness.evaluation.reasons.iter().any(|reason| {
        matches!(
            reason,
            crate::ReadinessReason::WorkspaceUnknownDirty {
                event_id: Some(event_id)
            } if event_id == "task-step-durable-mutation-replay-failed:task_1:step_1"
        )
    }));
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let workspace = std::fs::canonicalize(workspace)?;
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
    let mut policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    policy.required_checks = session
        .verification_state_projection()
        .check_specs_for_scopes(&[EvidenceScope::Task("task_1".to_owned())])
        .into_iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect();
    policy.completion_criteria = crate::CompletionCriteria::AllRequiredChecks;
    policy.timeout_ms = Some(60_000);
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            policy,
            "event-policy",
        )?,
    ))?;
    let output = StepRunOutput {
        final_text: "done".to_owned(),
        outcome: crate::AgentRunOutcome {
            changed_files: vec!["note.txt".to_owned()],
            ..crate::AgentRunOutcome::default()
        },

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
    assert!(run_task_step_verification_checks_with_fake_backend(
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
    let projection = session.verification_state_projection();
    assert!(projection.receipts.len() == 1);
    let check_run_entries = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::VerificationCheckRun(entry)) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        check_run_entries
            .iter()
            .map(|entry| entry.status)
            .collect::<Vec<_>>(),
        vec![
            crate::VerificationCheckRunStatus::Queued,
            crate::VerificationCheckRunStatus::Running,
            crate::VerificationCheckRunStatus::Succeeded,
        ]
    );
    assert!(
        check_run_entries
            .iter()
            .all(|entry| entry.timeout_ms == Some(60_000))
    );
    assert_eq!(projection.check_runs.len(), 1);
    let latest_run = projection
        .check_runs
        .values()
        .next()
        .expect("check run should project latest state");
    assert_eq!(
        latest_run.status,
        crate::VerificationCheckRunStatus::Succeeded
    );
    assert_eq!(latest_run.timeout_ms, Some(60_000));
    assert!(latest_run.receipt_id.is_some());
    Ok(())
}

#[test]
fn exact_task_verification_rerun_reuses_durable_check_lifecycle() -> Result<()> {
    let TaskVerificationRerunFixture {
        _temp,
        workspace,
        mut session,
        request,
    } = task_verification_rerun_fixture()?;
    let mut handler = RecordingEventHandler::default();
    let backend = FakeTaskExecutionBackend;

    let output = futures::executor::block_on(rerun_task_verification_check(
        &mut session,
        &mut handler,
        &backend,
        &workspace,
        &request,
    ))?;

    assert_eq!(
        output.check_run.status,
        crate::VerificationCheckRunStatus::Succeeded
    );
    assert_eq!(
        output.verification.receipt.binding.check_spec_hash,
        request.check_spec_hash
    );
    assert_eq!(
        output.verification.receipt.binding.workspace_snapshot_id,
        request.workspace_snapshot_id
    );
    let statuses = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::VerificationCheckRun(run)) => Some(run.status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            crate::VerificationCheckRunStatus::Queued,
            crate::VerificationCheckRunStatus::Running,
            crate::VerificationCheckRunStatus::Succeeded,
        ]
    );
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::Control(_)))
            .count(),
        4
    );
    let error = futures::executor::block_on(rerun_task_verification_check(
        &mut session,
        &mut handler,
        &backend,
        &workspace,
        &request,
    ))
    .expect_err("a successful rendered binding must not execute twice");
    assert!(error.to_string().contains("already succeeded"));
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::VerificationCheckRun(_))
            ))
            .count(),
        3
    );
    Ok(())
}

#[test]
fn exact_task_verification_rerun_rejects_workspace_drift_before_queue() -> Result<()> {
    let TaskVerificationRerunFixture {
        _temp,
        workspace,
        mut session,
        request,
    } = task_verification_rerun_fixture()?;
    std::fs::write(workspace.join("note.txt"), "changed after render\n")?;
    let mut handler = RecordingEventHandler::default();
    let backend = FakeTaskExecutionBackend;

    let error = futures::executor::block_on(rerun_task_verification_check(
        &mut session,
        &mut handler,
        &backend,
        &workspace,
        &request,
    ))
    .expect_err("workspace drift must reject the rendered rerun binding");

    assert!(error.to_string().contains("workspace changed"));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::VerificationCheckRun(_))
    )));
    assert!(handler.events.is_empty());
    Ok(())
}

#[test]
fn exact_task_verification_rerun_rejects_policy_and_spec_drift() -> Result<()> {
    let TaskVerificationRerunFixture {
        _temp,
        workspace,
        mut session,
        request,
    } = task_verification_rerun_fixture()?;
    let mut changed_policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    changed_policy.allow_unverified_completion = true;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(
            EvidenceScope::Task(request.task_id.as_str().to_owned()),
            changed_policy,
            "event-policy-changed",
        )?,
    ))?;
    let mut handler = RecordingEventHandler::default();
    let backend = FakeTaskExecutionBackend;
    let error = futures::executor::block_on(rerun_task_verification_check(
        &mut session,
        &mut handler,
        &backend,
        &workspace,
        &request,
    ))
    .expect_err("policy drift must reject the rendered rerun binding");
    assert!(error.to_string().contains("policy changed"));

    let TaskVerificationRerunFixture {
        _temp,
        workspace,
        mut session,
        mut request,
    } = task_verification_rerun_fixture()?;
    request.check_spec_hash = "stale-check-spec-hash".to_owned();
    let error = futures::executor::block_on(rerun_task_verification_check(
        &mut session,
        &mut handler,
        &backend,
        &workspace,
        &request,
    ))
    .expect_err("check spec drift must reject the rendered rerun binding");
    assert!(error.to_string().contains("verification check changed"));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::VerificationCheckRun(_))
    )));
    Ok(())
}

#[test]
fn task_step_auto_run_policy_defaults_manual_and_reads_recorded_policy() -> Result<()> {
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    };
    let temp = tempfile::tempdir()?;
    let mut options = options();
    options.workspace_root = temp.path().to_path_buf();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");

    assert_eq!(
        task_step_auto_run_policy(&session, &request, &step, &options)?,
        VerificationAutoRunPolicy::Manual
    );

    let mut task_policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    task_policy.auto_run = VerificationAutoRunPolicy::TrustedOnly;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            task_policy,
            "event-policy-task",
        )?,
    ))?;
    assert_eq!(
        task_step_auto_run_policy(&session, &request, &step, &options)?,
        VerificationAutoRunPolicy::TrustedOnly
    );

    let mut step_policy = crate::VerificationPolicy::no_checks_required("task_step_default");
    step_policy.auto_run = VerificationAutoRunPolicy::Never;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(
            EvidenceScope::Step("task_1:step_1".to_owned()),
            step_policy,
            "event-policy-step",
        )?,
    ))?;
    assert_eq!(
        task_step_auto_run_policy(&session, &request, &step, &options)?,
        VerificationAutoRunPolicy::Never
    );
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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
    assert!(!run_task_step_verification_checks_with_fake_backend(
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
    assert!(!run_task_step_verification_checks_with_fake_backend(
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
    let error = run_task_step_verification_checks_with_fake_backend(
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
    let no_backend_error = futures::executor::block_on(run_task_step_verification_checks(
        &mut session,
        &mut handler,
        None,
        &request,
        &step,
        &options,
        &failed_check,
    ))
    .expect_err("check execution should fail closed without a backend");
    assert!(
        no_backend_error
            .to_string()
            .contains("requires an execution backend")
    );
    assert!(run_task_step_verification_checks_with_fake_backend(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &failed_check,
    )?);
    let projection = session.verification_state_projection();
    let receipts = &projection.receipts;
    assert!(
        receipts
            .values()
            .any(|entry| entry.receipt.check_status == crate::ReceiptStatus::Failed)
    );
    assert!(receipts.values().any(|entry| {
        entry.receipt.receipt.policy_hash.as_deref() == Some(expected_policy_hash.as_str())
    }));
    assert!(projection.check_runs.values().any(|entry| {
        entry.status == crate::VerificationCheckRunStatus::Failed
            && entry.check_spec_id == "always-fails"
            && entry.receipt_id.is_some()
    }));

    let spawn_error = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["--version".to_owned()],
            cwd: Some(PathBuf::from("missing-cwd")),
        },
        source_event_id: "event-config-spawn-error".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "spawn-error",
        "task_step_default",
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config-spawn-error".to_owned(),
        },
    )?;
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(
            EvidenceScope::Task("task_1".to_owned()),
            spawn_error,
            "event-config-spawn-error",
        ),
    ))?;
    let spawn_error_readiness = crate::ReadinessEvaluatedEntry {
        evaluation: crate::ReadinessEvaluation {
            required_actions: vec![crate::RequiredAction::RunCheck {
                check_spec_id: "spawn-error".to_owned(),
            }],
            ..no_action.evaluation.clone()
        },
        ..no_action
    };
    let spawn_error = run_task_step_verification_checks_with_fake_backend(
        &mut session,
        &mut handler,
        &request,
        &step,
        &options,
        &spawn_error_readiness,
    )
    .expect_err("spawn failure should keep the task blocked");
    assert!(spawn_error.to_string().contains("failed to spawn"));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::VerificationCheckRun(run))
                if run.check_spec_id == "spawn-error"
                    && run.status == crate::VerificationCheckRunStatus::Errored
                    && run
                        .reason
                        .as_deref()
                        .is_some_and(|reason| reason.contains("failed to spawn"))
        )
    }));
    Ok(())
}

#[test]
fn task_step_status_completes_when_only_verification_config_is_missing() -> Result<()> {
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        TaskStepStatus::Completed
    );
    assert!(
        readiness
            .evaluation
            .required_actions
            .iter()
            .any(|action| matches!(action, crate::RequiredAction::ProvideVerificationConfig))
    );
    let run_check_readiness = crate::ReadinessEvaluatedEntry {
        evaluation: crate::ReadinessEvaluation {
            required_actions: vec![crate::RequiredAction::RunCheck {
                check_spec_id: "docs-check".to_owned(),
            }],
            ..readiness.evaluation.clone()
        },
        ..readiness
    };
    assert_eq!(
        step_status_after_readiness(TaskStepStatus::Completed, &run_check_readiness),
        TaskStepStatus::Blocked
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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
        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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
        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
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

        changeset_proposal: None,
        changeset_only_after_snapshot_id: None,
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
fn durable_workspace_mutation_evidence_replays_stored_events() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&log_path)?;
    store.append_event(
        DurableEventType::MutationPrepared,
        EventClass::Critical,
        serde_json::to_value(MutationPrepared {
            operation_id: "op-commit".to_owned(),
            batch_id: None,
            tool_call_id: Some("call-file".to_owned()),
            causation_event_id: "event-tool-started".to_owned(),
            subject: MutationSubject::File {
                path: "note.txt".into(),
                file_type: FileType::File,
            },
            before_hash: None,
            intended_after_hash: None,
            snapshot_coverage: SnapshotCoverage::NoPriorContent,
            workspace_id: "workspace-1".to_owned(),
            base_workspace_revision: 1,
            sync_class: MutationSyncClass::RecoveryCritical,
        })?,
    )?;
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
    store.append_event(
        DurableEventType::MutationPrepared,
        EventClass::Critical,
        serde_json::to_value(MutationPrepared {
            operation_id: "op-reconcile".to_owned(),
            batch_id: None,
            tool_call_id: Some("call-file".to_owned()),
            causation_event_id: "event-tool-started".to_owned(),
            subject: MutationSubject::File {
                path: "note.txt".into(),
                file_type: FileType::File,
            },
            before_hash: None,
            intended_after_hash: Some("sha256:intended".to_owned()),
            snapshot_coverage: SnapshotCoverage::Captured("artifact-before".to_owned()),
            workspace_id: "workspace-1".to_owned(),
            base_workspace_revision: 2,
            sync_class: MutationSyncClass::RecoveryCritical,
        })?,
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
    let precise_detected = store.append_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        serde_json::to_value(WorkspaceMutationDetected {
            operation_id: "op-detected".to_owned(),
            tool_call_id: Some("call-bash".to_owned()),
            tool_name: "bash".to_owned(),
            tool_effect: ToolEffect::Unknown,
            workspace_id: "workspace-1".to_owned(),
            scope_hash: "scope-main".to_owned(),
            from_workspace_snapshot_id: Some("snapshot-before".to_owned()),
            to_workspace_snapshot_id: Some("snapshot-after".to_owned()),
            base_workspace_revision: 3,
            workspace_revision: 4,
            reason: WorkspaceMutationDetectionReason::SnapshotChanged,
            unknown_dirty: false,
            metadata: Default::default(),
        })?,
    )?;
    let mcp_detected = store.append_event(
        DurableEventType::WorkspaceMutationDetected,
        EventClass::Critical,
        serde_json::to_value(WorkspaceMutationDetected {
            operation_id: "op-mcp".to_owned(),
            tool_call_id: None,
            tool_name: "mcp_server:docs".to_owned(),
            tool_effect: ToolEffect::Unknown,
            workspace_id: "workspace-1".to_owned(),
            scope_hash: "scope-main".to_owned(),
            from_workspace_snapshot_id: None,
            to_workspace_snapshot_id: None,
            base_workspace_revision: 4,
            workspace_revision: 5,
            reason: WorkspaceMutationDetectionReason::DeclaredWriteEffect,
            unknown_dirty: true,
            metadata: Default::default(),
        })?,
    )?;
    let restored = store.append_event(
        DurableEventType::CheckpointRestored,
        EventClass::Critical,
        serde_json::to_value(CheckpointRestored {
            operation_id: "op-restore".to_owned(),
            batch_id: None,
            tool_call_id: Some("call-restore".to_owned()),
            restored_subject: MutationSubject::File {
                path: "note.txt".into(),
                file_type: FileType::File,
            },
            restored_from: SnapshotCoverage::Captured("artifact-before".to_owned()),
            mutation_committed_event_id: "event-restore-commit".to_owned(),
            workspace_revision: 5,
            workspace_snapshot_id: "snapshot-restored".to_owned(),
        })?,
    )?;
    let child_merge = store.append_event(
        DurableEventType::ChildChangesetMerged,
        EventClass::Critical,
        json!({
            "changeset_id": "changeset-1",
            "parent_workspace_snapshot_before_id": "snapshot-parent-before",
            "parent_workspace_snapshot_after_id": "snapshot-parent-after",
        }),
    )?;
    let agent_merge_unknown = store.append_event(
        DurableEventType::AgentMergeApplied,
        EventClass::Critical,
        json!({
            "agent_thread_id": "agent-1"
        }),
    )?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let evidence = durable_workspace_mutation_evidence(
        &session,
        &TaskId::new("task_1")?,
        &crate::VerificationScope::all_tracked("scope-main"),
        &[
            "call-file".to_owned(),
            "call-bash".to_owned(),
            "call-restore".to_owned(),
        ],
        0,
    )?;

    assert_eq!(evidence.len(), 8);
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
    assert_eq!(evidence[3].event_id, precise_detected.event_id);
    assert_eq!(
        evidence[3].from_workspace_snapshot_id.as_deref(),
        Some("snapshot-before")
    );
    assert_eq!(
        evidence[3].to_workspace_snapshot_id.as_deref(),
        Some("snapshot-after")
    );
    assert!(!evidence[3].unknown_dirty);
    assert_eq!(evidence[4].event_id, mcp_detected.event_id);
    assert_eq!(evidence[4].source_label.as_deref(), Some("MCP server docs"));
    assert_eq!(
        evidence[4].recovery_hint.as_deref(),
        Some("refresh MCP or run check")
    );
    assert!(evidence[4].unknown_dirty);
    assert_eq!(evidence[5].event_id, restored.event_id);
    assert_eq!(evidence[5].source_event_type, "checkpoint_restored");
    assert_eq!(
        evidence[5].to_workspace_snapshot_id.as_deref(),
        Some("snapshot-restored")
    );
    assert!(!evidence[5].unknown_dirty);
    assert_eq!(evidence[6].event_id, child_merge.event_id);
    assert_eq!(evidence[6].source_event_type, "child_changeset_merged");
    assert_eq!(
        evidence[6].from_workspace_snapshot_id.as_deref(),
        Some("snapshot-parent-before")
    );
    assert_eq!(
        evidence[6].to_workspace_snapshot_id.as_deref(),
        Some("snapshot-parent-after")
    );
    assert!(!evidence[6].unknown_dirty);
    assert_eq!(evidence[7].event_id, agent_merge_unknown.event_id);
    assert_eq!(evidence[7].source_event_type, "agent_merge_applied");
    assert!(evidence[7].unknown_dirty);
    Ok(())
}

#[test]
fn durable_workspace_mutation_evidence_marks_open_execution_unknown_dirty() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&log_path)?;
    let profile = test_execution_profile("call-shell", "bash", "snapshot-before", 7);
    let started = store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
            call_id: "call-shell".to_owned(),
            tool_name: "bash".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta {
                details: json!({ "execution_mutation_profile": profile }),
                ..Default::default()
            },
            error: None,
            model_content_hash: None,
        })),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let evidence = durable_workspace_mutation_evidence(
        &session,
        &TaskId::new("task_1")?,
        &crate::VerificationScope::all_tracked("scope-main"),
        &["call-shell".to_owned()],
        0,
    )?;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].event_id, started.event_id);
    assert_eq!(evidence[0].source_event_type, "running_tool_execution");
    assert_eq!(
        evidence[0].from_workspace_snapshot_id.as_deref(),
        Some("snapshot-before")
    );
    assert!(evidence[0].unknown_dirty);
    Ok(())
}

#[test]
fn durable_workspace_mutation_evidence_marks_running_terminal_task_unknown_dirty() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&log_path)?;
    let profile = test_execution_profile("call-terminal", "terminal_start", "snapshot-before", 11);
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-terminal".to_owned(),
            tool_name: "terminal_start".to_owned(),
            status: ToolExecutionStatus::Started,
            duration_ms: None,
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta {
                details: json!({ "execution_mutation_profile": profile }),
                ..Default::default()
            },
            error: None,
            model_content_hash: None,
        }),
    )))?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::ToolExecution(
        Box::new(ToolExecutionEntry {
            call_id: "call-terminal".to_owned(),
            tool_name: "terminal_start".to_owned(),
            status: ToolExecutionStatus::Completed,
            duration_ms: Some(1),
            subjects: Vec::new(),
            changed_files: Vec::new(),
            metadata: ToolResultMeta {
                details: json!({ "task_id": "terminal-1" }),
                ..Default::default()
            },
            error: None,
            model_content_hash: None,
        }),
    )))?;
    let terminal_running =
        terminal_task_entry(temp.path(), "terminal-1", TerminalTaskStatus::Running, 20)?;
    let terminal_event = store.append_session_entry_event(&SessionLogEntry::Control(
        ControlEntry::TerminalTask(terminal_running),
    ))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store.clone());

    let evidence = durable_workspace_mutation_evidence(
        &session,
        &TaskId::new("task_1")?,
        &crate::VerificationScope::all_tracked("scope-main"),
        &["call-terminal".to_owned()],
        0,
    )?;

    assert_eq!(evidence.len(), 1);
    assert_eq!(evidence[0].event_id, terminal_event.event_id);
    assert_eq!(evidence[0].source_event_type, "running_terminal_task");
    assert_eq!(
        evidence[0].from_workspace_snapshot_id.as_deref(),
        Some("snapshot-before")
    );
    assert!(evidence[0].unknown_dirty);

    let terminal_exited = terminal_task_entry(
        temp.path(),
        "terminal-1",
        TerminalTaskStatus::Exited { exit_code: Some(0) },
        30,
    )?;
    store.append_session_entry_event(&SessionLogEntry::Control(ControlEntry::TerminalTask(
        terminal_exited,
    )))?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);
    let clean = durable_workspace_mutation_evidence(
        &session,
        &TaskId::new("task_1")?,
        &crate::VerificationScope::all_tracked("scope-main"),
        &["call-terminal".to_owned()],
        0,
    )?;

    assert!(clean.is_empty());
    Ok(())
}

#[test]
fn latest_relevant_successful_verification_sequence_ignores_unrelated_receipts() -> Result<()> {
    let trusted = CandidateCheck {
        source: CheckDiscoverySource::UserExplicitConfig,
        command: CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        source_event_id: "event-config".to_owned(),
        workspace_trust_snapshot_id: "user-config".to_owned(),
    }
    .promote(
        "cargo-test",
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        ToolEffect::ReadOnly,
        CheckPromotion::ExplicitUserConfig {
            config_event_id: "event-config".to_owned(),
        },
    )?;
    let policy = crate::VerificationPolicy {
        required_checks: vec![trusted.check_spec.clone()],
        completion_criteria: crate::CompletionCriteria::AllRequiredChecks,
        verification_scope: crate::VerificationScope::all_tracked(
            DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        ),
        sandbox_profile: crate::SandboxProfileRequirement::None,
        workspace_trust_requirement: crate::WorkspaceTrustRequirement::None,
        allow_unverified_completion: false,
        timeout_ms: None,
        auto_run: crate::VerificationAutoRunPolicy::Manual,
    };
    let policy_hash = policy.stable_hash()?;
    let relevant_scope = EvidenceScope::Task("task_1".to_owned());
    let unrelated_scope = EvidenceScope::Task("task_other".to_owned());
    let mut projection = crate::VerificationStateProjection::default();
    projection.receipts.insert(
        "receipt-unrelated".to_owned(),
        crate::VerificationRecordedEntry {
            receipt: task_test_verification_receipt(
                &trusted.check_spec,
                unrelated_scope,
                Some(policy_hash.clone()),
                90,
            ),
        },
    );
    projection.receipts.insert(
        "receipt-wrong-policy".to_owned(),
        crate::VerificationRecordedEntry {
            receipt: task_test_verification_receipt(
                &trusted.check_spec,
                relevant_scope.clone(),
                Some("other-policy".to_owned()),
                95,
            ),
        },
    );
    projection.receipts.insert(
        "receipt-relevant".to_owned(),
        crate::VerificationRecordedEntry {
            receipt: task_test_verification_receipt(
                &trusted.check_spec,
                relevant_scope.clone(),
                Some(policy_hash.clone()),
                42,
            ),
        },
    );

    let sequence = latest_relevant_successful_verification_sequence(
        &projection,
        std::slice::from_ref(&relevant_scope),
        &policy,
        &policy_hash,
    );

    assert_eq!(sequence, 42);
    let receipts =
        relevant_verification_receipts(&projection, &[relevant_scope], &policy, &policy_hash);
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].receipt.recorded_at_stream_sequence, 42);
    Ok(())
}

fn append_trusted_only_policy_for_task(session: &mut Session, task_id: &str) -> Result<()> {
    let task_scope = EvidenceScope::Task(task_id.to_owned());
    let required_checks = session
        .verification_state_projection()
        .check_specs_for_scopes(std::slice::from_ref(&task_scope))
        .into_iter()
        .map(|entry| entry.trusted_check.check_spec.clone())
        .collect::<Vec<_>>();
    let mut policy =
        crate::VerificationPolicy::no_checks_required(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    policy.required_checks = required_checks;
    policy.completion_criteria = crate::CompletionCriteria::AllRequiredChecks;
    policy.allow_unverified_completion = false;
    policy.auto_run = VerificationAutoRunPolicy::TrustedOnly;
    session.append_control(ControlEntry::VerificationPolicyChanged(
        crate::VerificationPolicyChangedEntry::new(task_scope, policy, "event-auto-run-policy")?,
    ))?;
    Ok(())
}

#[test]
fn task_step_default_policy_preserves_repo_check_trust_requirement() -> Result<()> {
    let check_spec = CheckSpec::new(
        "cargo-test",
        CheckCommand {
            command: "cargo".to_owned(),
            args: vec!["test".to_owned()],
            cwd: None,
        },
        ToolEffect::ReadOnly,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    );
    let trusted = TrustedCheckSpec {
        check_spec,
        source: CheckDiscoverySource::Cargo,
        workspace_trust_snapshot_id: "trust-1".to_owned(),
        promoted_by: CheckPromotion::WorkspaceTrusted {
            trust_event_id: "event-trust".to_owned(),
        },
        approval_event_id: None,
        sandbox_decision_id: None,
    };
    let mut session = Session::new("planner", "model");
    let task_scope = EvidenceScope::Task("task_1".to_owned());
    let step_scope = EvidenceScope::Step("task_1:step_1".to_owned());
    let workspace_scope = EvidenceScope::Workspace("workspace-main".to_owned());
    session.append_control(ControlEntry::CheckSpecRecorded(
        CheckSpecRecordedEntry::new(task_scope.clone(), trusted, "event-discovery"),
    ))?;
    let projection = session.verification_state_projection();

    let policy = task_step_default_policy(&projection, &step_scope, &task_scope, &workspace_scope);

    assert_eq!(
        policy.workspace_trust_requirement,
        crate::WorkspaceTrustRequirement::Trusted
    );
    Ok(())
}

fn task_test_verification_receipt(
    check: &crate::CheckSpec,
    scope: EvidenceScope,
    policy_hash: Option<String>,
    sequence: u64,
) -> crate::VerificationReceipt {
    crate::VerificationReceipt {
        receipt: crate::EvidenceReceipt {
            receipt_id: format!("receipt-{sequence}"),
            source_session_id: "session-1".to_owned(),
            source_event_id: format!("event-{sequence}"),
            source_event_type: DurableEventType::CheckFinished.as_str().to_owned(),
            scope,
            producer_tool_call: None,
            workspace_revision: Some(sequence),
            workspace_snapshot_id: Some("snapshot-current".to_owned()),
            policy_hash,
            changeset_id: None,
            status: crate::ReceiptStatus::Succeeded,
            artifact_refs: Vec::new(),
            redaction_state: crate::RedactionState::None,
            recorded_at_stream_sequence: sequence,
        },
        binding: crate::VerificationBinding {
            workspace_id: "workspace-1".to_owned(),
            workspace_snapshot_id: "snapshot-current".to_owned(),
            verification_scope_hash: DEFAULT_TASK_VERIFICATION_SCOPE_HASH.to_owned(),
            check_spec_hash: check.check_spec_hash.clone(),
            environment_fingerprint: "env".to_owned(),
            sandbox_profile_hash: "sandbox".to_owned(),
            execution_backend: None,
            execution_backend_capabilities: None,
            execution_network: Default::default(),
            workspace_trust_snapshot_id: "trust".to_owned(),
            approval_event_id: None,
            sandbox_decision_id: None,
        },
        check_spec_id: check.check_spec_id.clone(),
        check_status: crate::ReceiptStatus::Succeeded,
        failure_reason: None,
        mutates_verification_scope: false,
    }
}

fn test_execution_profile(
    call_id: &str,
    tool_name: &str,
    snapshot_id: &str,
    workspace_revision: u64,
) -> ExecutionMutationProfile {
    ExecutionMutationProfile {
        tool_call_id: call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        effect: ToolEffect::Unknown,
        workspace_id: "workspace-1".to_owned(),
        scan_scope_hash: "scope-main".to_owned(),
        pre_execution_snapshot_id: Some(snapshot_id.to_owned()),
        pre_execution_workspace_revision: workspace_revision,
        workspace_knowledge: WorkspaceKnowledge::Clean(workspace_revision),
    }
}

fn terminal_task_entry(
    root: &std::path::Path,
    task_id: &str,
    status: TerminalTaskStatus,
    updated_at_ms: u64,
) -> Result<TerminalTaskEntry> {
    Ok(TerminalTaskEntry {
        handle: TerminalTaskHandle {
            task_id: TerminalTaskId::new(task_id)?,
            command: "sleep 60".to_owned(),
            cwd: root.to_path_buf(),
            shell: "zsh".to_owned(),
            log_path: root.join(format!("{task_id}.log")),
            created_at_ms: 1,
            execution_backend: None,
            execution_backend_capabilities: None,
            enforcement_backend: None,
            enforcement_backend_capabilities: None,
            sandbox_profile: None,
        },
        status,
        output_preview: None,
        output_hash: None,
        output_truncated: false,
        output_total_bytes: 0,
        output_limit_bytes: None,
        output_termination_reason: None,
        cleanup: None,
        updated_at_ms,
    })
}

#[test]
fn durable_workspace_mutation_evidence_errors_on_unreadable_stream() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let log_path = temp.path().join("session.jsonl");
    std::fs::write(&log_path, "{not-json}\n")?;
    let store = JsonlSessionStore::new(&log_path)?;
    let session = Session::new("deepseek", "deepseek-v4-flash").with_store(store);

    let error = durable_workspace_mutation_evidence(
        &session,
        &TaskId::new("task_1")?,
        &crate::VerificationScope::all_tracked("scope-main"),
        &["call-shell".to_owned()],
        0,
    )
    .expect_err("corrupt durable stream should not be treated as empty evidence");
    assert!(error.to_string().contains("failed to parse"));
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
    assert_eq!(
        run_status_from_step_status(TaskStepStatus::Superseded),
        crate::RunStatus::Cancelled
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
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
            },
            TaskStepSpec {
                step_id: TaskStepId::new("step_2")?,
                title: "remaining".to_owned(),
                display_name: None,
                detail: None,
                role: crate::AgentRole::Executor,
                depends_on: Vec::new(),
                mode: None,
                isolation: None,
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

fn seed_task_with_steps(
    session: &mut Session,
    status: TaskRunStatus,
    steps: Vec<TaskStepSpec>,
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
        steps,
        reason: None,
    }))?;
    Ok(())
}

fn read_executor_step(
    step_id: &str,
    title: &str,
    depends_on: Vec<TaskStepId>,
) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(step_id)?,
        title: title.to_owned(),
        display_name: None,
        detail: None,
        role: crate::AgentRole::Executor,
        depends_on,
        mode: Some(TaskStepMode::Read),
        isolation: Some(TaskIsolationMode::SharedReadOnly),
    })
}

fn task_step_entry_index(
    entries: &[SessionLogEntry],
    step_id: &str,
    status: TaskStepStatus,
) -> Option<usize> {
    entries.iter().position(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskStep(step))
                if step.step_id.as_str() == step_id && step.status == status
        )
    })
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
            depends_on: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;
    Ok(())
}

fn changeset_only_step() -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new("changeset_step")?,
        title: "propose change".to_owned(),
        display_name: None,
        detail: Some("produce a changeset proposal".to_owned()),
        role: crate::AgentRole::SubagentWrite,
        depends_on: Vec::new(),
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::ChangesetOnly),
    })
}

fn changeset_only_child_final_text(change_id: &str) -> String {
    format!(
        r#"```sigil_changeset
{{
  "change_set": {{
    "id": "{change_id}",
    "title": "Update note",
    "summary": "Would update note.txt",
    "risk": "low",
    "files": [
      {{
        "path": "note.txt",
        "action": "update",
        "risk": "low",
        "additions": 1,
        "deletions": 1
      }}
    ],
    "validations": []
  }}
,
  "artifact": {{
    "media_type": "text/x-diff",
    "content": "--- a/note.txt\n+++ b/note.txt\n@@\n-old\n+new\n"
  }}
}}
```"#
    )
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

fn options_for_workspace(workspace_root: &std::path::Path) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: workspace_root.to_path_buf(),
        ..options()
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
