use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    pin::Pin,
    process::Command,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures::{Stream, stream};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    Agent, AgentConfig, AgentDelegationAdmissionEntry, AgentFinalAnswerRef, AgentInvocationGrant,
    AgentInvocationGrantBinding, AgentInvocationGrantSource, AgentInvocationMode,
    AgentInvocationSource, AgentRole, AgentRouteId, AgentRunInput, AgentRunOptions,
    AgentRunOutcome, AgentRunTerminalReason, AgentThreadId, AgentThreadTerminalStatus,
    AgentUsageSummary, ApprovalMode, AutoApproveHandler, ChangeSet, ChangeSetFile,
    ChangeSetFileAction, ChangeSetId, ChangeSetRisk, CompactionConfig, CompletionRequest,
    ControlEntry, ConversationInputQueueId, DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
    DelegationAuthority, DelegationAuthorityRecord, EventHandler, IntegrationBaseRepresentation,
    IntegrationContentClass, IntegrationEffect, IntegrationObservedEffect, IntegrationPlanId,
    IntegrationProposalFacts, IntegrationProposalSpec, InteractionMode,
    IsolatedWorkspaceCleanupStatus, JsonlSessionStore, MemoryConfig, MessageRole, ModelMessage,
    MultiAgentMode, NetworkPolicy, PermissionConfig, Provider, ProviderCapabilities, ProviderChunk,
    ProviderPhysicalAttemptOutcome, ProviderRateLimitError, ProviderRequestRejection,
    ReasoningStreamSupport, RootConfig, RunCancellationOwner, RunEvent, Session, SessionConfig,
    SessionLogEntry, SessionRef, TASK_GUIDANCE_APPLY_TOOL_NAME, TASK_PLAN_UPDATE_TOOL_NAME,
    TaskChildChangeSetArtifact, TaskChildChangeSetProposal, TaskChildSessionBatchCommitEnvelope,
    TaskChildSessionBatchPreparation, TaskChildSessionRunRequest, TaskChildSessionRunner,
    TaskChildSessionStatus, TaskGuidanceAssessmentContext, TaskId, TaskIntegrationProposal,
    TaskIntegrationRunRequest, TaskIsolationMode, TaskParticipantAttemptId, TaskParticipantPurpose,
    TaskParticipantRetryError, TaskParticipantRetryProof, TaskPlanEntry, TaskPlanStatus,
    TaskPlanUpdateContext, TaskPlannerSessionRunRequest, TaskPlannerWorktreeAvailability,
    TaskRouteStatus, TaskStepId, TaskStepMode, TaskStepSpec, TaskSubagentApprovalRouteEntry,
    TaskSynthesisSessionRunRequest, Tool, ToolAccess, ToolCall, ToolCategory, ToolContext,
    ToolError, ToolErrorKind, ToolExecutionEntry, ToolExecutionStatus, ToolPreviewCapability,
    ToolRegistry, ToolRegistryScope, ToolResult, ToolResultMeta, ToolSpec, UsageStats,
    VerificationScope, WorkspaceConfig, WriteIsolationMode, build_integration_plan,
    build_workspace_snapshot, child_session_ref, decode_changeset_only_child_output,
    stable_workspace_id, task_participant_attempt_id, task_participant_logical_run_id,
    task_participant_session_ref,
};

use super::{
    AgentBudgetPolicy, AgentChatChildStart, AgentMailboxMessage, AgentProfileRegistry,
    AgentResultMaterialization, AgentSupervisor, AgentSupervisorTaskChildRunner,
    AgentTaskChildStart, REQUEST_TASK_DISCOVERY_TOOL_NAME, agent_terminal_status_from_task_child,
    planner_tools_with_discovery, task_child_status_from_outcome,
    task_runner::bind_child_integration_facts, tool_scope_is_write_capable,
};
use crate::{AgentToolRuntime, EXPLORE_PROFILE_ID};

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

#[derive(Default)]
struct RejectApprovalPresentationHandler {
    requests: usize,
}

impl EventHandler for RejectApprovalPresentationHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        if matches!(event, RunEvent::ToolApprovalRequested { .. }) {
            self.requests = self.requests.saturating_add(1);
            anyhow::bail!("approval presenter unavailable");
        }
        Ok(())
    }
}

#[derive(Default)]
struct CountingApprovalHandler {
    decisions: usize,
}

impl sigil_kernel::ApprovalHandler for CountingApprovalHandler {
    fn approve_tool_call(
        &mut self,
        _call: &ToolCall,
        _spec: &ToolSpec,
    ) -> Result<sigil_kernel::ToolApproval> {
        self.decisions = self.decisions.saturating_add(1);
        Ok(sigil_kernel::ToolApproval::Approve)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
    }
}

fn participant_attempt_id_for(step_id: &str) -> Result<TaskParticipantAttemptId> {
    task_participant_attempt_id(
        &TaskId::new("task_1")?,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&TaskStepId::new(step_id)?),
        1,
    )
}

fn participant_session_ref_for(step_id: &str) -> Result<SessionRef> {
    let task_id = TaskId::new("task_1")?;
    let attempt_id = participant_attempt_id_for(step_id)?;
    task_participant_session_ref(&task_id, &attempt_id)
}

struct TextProvider {
    text: &'static str,
}

struct GuidanceApplyPlannerProvider;

struct PlannerDiscoveryProvider {
    observed_results: Arc<Mutex<Option<String>>>,
}

struct ParallelDiscoveryProvider {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
}

struct RejectedDiscoveryPlannerProvider {
    observed_error: Arc<Mutex<Option<String>>>,
}

struct RepeatedDiscoveryPlannerProvider {
    observed_rejection: Arc<Mutex<Option<String>>>,
}

struct CountingDiscoveryProvider {
    starts: Arc<AtomicUsize>,
}

struct ParallelTaskChildProvider {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    completion_order: Arc<Mutex<Vec<String>>>,
}

struct ParallelChangesetChildProvider {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    completion_order: Arc<Mutex<Vec<String>>>,
}

struct ParallelWorktreeWriteProvider {
    barrier: Arc<tokio::sync::Barrier>,
    active: Arc<AtomicUsize>,
    max_active: Arc<AtomicUsize>,
    completion_order: Arc<Mutex<Vec<String>>>,
}

struct RateLimitedTaskChildProvider {
    starts: Arc<AtomicUsize>,
    rate_limits_remaining: Arc<AtomicUsize>,
}

#[derive(Debug, thiserror::Error)]
#[error("task child connect failed before dispatch")]
struct TaskChildConnectFailedBeforeDispatch;

struct ConnectThenRateLimitedTaskChildProvider {
    starts: Arc<AtomicUsize>,
}

struct OutputThenRateLimitedTaskChildProvider;

#[async_trait]
impl Provider for TextProvider {
    fn name(&self) -> &str {
        "text"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(self.text.to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for GuidanceApplyPlannerProvider {
    fn name(&self) -> &str {
        "guidance-apply-planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let args = r#"{"reason":"clarifies_existing_step","target_step_ids":["step_1"]}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-guidance-apply".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-guidance-apply".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-guidance-apply".to_owned(),
                name: TASK_GUIDANCE_APPLY_TOOL_NAME.to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ParallelTaskChildProvider {
    fn name(&self) -> &str {
        "parallel-task-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let step_id = ["read_a", "read_b"]
            .into_iter()
            .find(|step_id| {
                request.messages.iter().any(|message| {
                    message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(step_id))
                })
            })
            .ok_or_else(|| anyhow!("parallel child request did not identify a test step"))?;
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait().await;
        if step_id == "read_a" {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        self.completion_order
            .lock()
            .expect("completion order should not be poisoned")
            .push(step_id.to_owned());
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("parallel read done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ParallelChangesetChildProvider {
    fn name(&self) -> &str {
        "parallel-changeset-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let step_id = ["proposal_a", "proposal_b"]
            .into_iter()
            .find(|step_id| {
                request.messages.iter().any(|message| {
                    message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(step_id))
                })
            })
            .ok_or_else(|| anyhow!("parallel changeset request did not identify a test step"))?;
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait().await;
        if step_id == "proposal_a" {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        self.completion_order
            .lock()
            .expect("completion order should not be poisoned")
            .push(step_id.to_owned());
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(changeset_output(step_id))),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ParallelWorktreeWriteProvider {
    fn name(&self) -> &str {
        "parallel-worktree-write"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool))
        {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "parallel isolated worktree edit complete".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        let step_id = ["worktree_a", "worktree_b"]
            .into_iter()
            .find(|step_id| {
                request.messages.iter().any(|message| {
                    message
                        .content
                        .as_deref()
                        .is_some_and(|content| content.contains(step_id))
                })
            })
            .ok_or_else(|| anyhow!("parallel worktree request did not identify a test step"))?;
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait().await;
        if step_id == "worktree_a" {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        }
        self.completion_order
            .lock()
            .expect("completion order should not be poisoned")
            .push(step_id.to_owned());
        self.active.fetch_sub(1, Ordering::SeqCst);
        let path = if step_id == "worktree_a" {
            "worktree_a.txt"
        } else {
            "worktree_b.txt"
        };
        let args = json!({
            "path": path,
            "content": format!("{step_id} isolated edit\n")
        })
        .to_string();
        let call_id = format!("call-{step_id}");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: "write_file".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: args.clone(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: "write_file".to_owned(),
                args_json: args,
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RateLimitedTaskChildProvider {
    fn name(&self) -> &str {
        "rate-limited-task-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        if self
            .rate_limits_remaining
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
        {
            return Err(ProviderRateLimitError::new(
                anyhow!("test provider rate limited"),
                Some("60"),
            )
            .into());
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("provider recovered".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ConnectThenRateLimitedTaskChildProvider {
    fn name(&self) -> &str {
        "connect-then-rate-limited-task-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        error
            .downcast_ref::<TaskChildConnectFailedBeforeDispatch>()
            .is_some()
            .then_some(ProviderRequestRejection::ConnectFailedBeforeDispatch)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.starts.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            return Err(TaskChildConnectFailedBeforeDispatch.into());
        }
        Err(ProviderRateLimitError::new(anyhow!("test provider rate limited"), Some("60")).into())
    }
}

#[async_trait]
impl Provider for OutputThenRateLimitedTaskChildProvider {
    fn name(&self) -> &str {
        "output-then-rate-limited-task-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("partial output".to_owned())),
            Err(
                ProviderRateLimitError::new(anyhow!("rate limited after output"), Some("1")).into(),
            ),
        ])))
    }
}

#[async_trait]
impl Provider for PlannerDiscoveryProvider {
    fn name(&self) -> &str {
        "planner-discovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if let Some(results) = request
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains(r#""type":"task_discovery_results""#))
        {
            *self
                .observed_results
                .lock()
                .expect("planner discovery observation lock should not be poisoned") =
                Some(results.to_owned());
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-plan-after-discovery".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "plan_version": 1,
                        "status": "accepted",
                        "steps": [{
                            "step_id": "implement",
                            "title": "Implement the verified change",
                            "role": "executor"
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }

        assert!(
            !request
                .messages
                .iter()
                .any(|message| matches!(message.role, MessageRole::Tool)),
            "planner should not receive a polling turn before discovery results"
        );
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-task-discovery".to_owned(),
                name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: json!({
                    "probes": [
                        {
                            "probe_id": "runtime",
                            "title": "Inspect runtime",
                            "objective": "Inspect runtime orchestration boundaries",
                            "path_hints": ["./"]
                        },
                        {
                            "probe_id": "kernel",
                            "title": "Inspect kernel",
                            "objective": "Inspect kernel task contracts",
                            "path_hints": null
                        }
                    ]
                })
                .to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ParallelDiscoveryProvider {
    fn name(&self) -> &str {
        "parallel-discovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.barrier.wait().await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        let scope = request
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("Assigned objective"))
            .unwrap_or("unknown discovery scope");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(format!(
                "discovery complete: {scope}"
            ))),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RejectedDiscoveryPlannerProvider {
    fn name(&self) -> &str {
        "rejected-discovery-planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if request
            .messages
            .iter()
            .filter_map(|message| message.content.as_deref())
            .any(|content| content.contains(r#""type":"task_discovery_results""#))
        {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-plan-after-corrected-discovery".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "plan_version": 1,
                        "status": "accepted",
                        "steps": [{
                            "step_id": "implement",
                            "title": "Implement after corrected research",
                            "role": "executor"
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        if let Some(error) = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("overlapping path hints"))
        {
            *self
                .observed_error
                .lock()
                .expect("planner discovery error observation lock should not be poisoned") =
                Some(error.to_owned());
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-corrected-discovery".to_owned(),
                    name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "probes": [{
                            "probe_id": "runtime-corrected",
                            "title": "Inspect runtime after correction",
                            "objective": "Inspect runtime orchestration after correcting the invalid batch",
                            "path_hints": ["crates/sigil-runtime"]
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-overlapping-discovery".to_owned(),
                name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: json!({
                    "probes": [
                        {
                            "probe_id": "runtime",
                            "title": "Inspect runtime",
                            "objective": "Inspect all runtime orchestration",
                            "path_hints": ["crates/sigil-runtime"]
                        },
                        {
                            "probe_id": "runtime-src",
                            "title": "Inspect runtime source",
                            "objective": "Inspect runtime source details",
                            "path_hints": ["crates/sigil-runtime/src"]
                        }
                    ]
                })
                .to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RepeatedDiscoveryPlannerProvider {
    fn name(&self) -> &str {
        "repeated-discovery-planner"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if let Some(rejection) = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .filter_map(|message| message.content.as_deref())
            .find(|content| content.contains("at most once per planning attempt"))
        {
            *self
                .observed_rejection
                .lock()
                .expect("planner discovery rejection lock should not be poisoned") =
                Some(rejection.to_owned());
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-plan-after-repeat".to_owned(),
                    name: TASK_PLAN_UPDATE_TOOL_NAME.to_owned(),
                    args_json: json!({
                        "plan_version": 1,
                        "status": "accepted",
                        "steps": [{
                            "step_id": "implement",
                            "title": "Implement after one research round",
                            "role": "executor"
                        }]
                    })
                    .to_string(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }

        let has_discovery_results = request
            .messages
            .iter()
            .filter(|message| matches!(message.role, MessageRole::Tool))
            .filter_map(|message| message.content.as_deref())
            .any(|content| content.contains(r#""type":"task_discovery_results""#));
        let call_id = if has_discovery_results {
            "call-repeat-discovery"
        } else {
            "call-initial-discovery"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id.to_owned(),
                name: REQUEST_TASK_DISCOVERY_TOOL_NAME.to_owned(),
                args_json: json!({
                    "probes": [{
                        "probe_id": "runtime",
                        "title": "Inspect runtime",
                        "objective": "Inspect runtime orchestration",
                        "path_hints": ["crates/sigil-runtime"]
                    }]
                })
                .to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for CountingDiscoveryProvider {
    fn name(&self) -> &str {
        "counting-discovery"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "unexpected discovery start".to_owned(),
            )),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct FailingProvider;

#[async_trait]
impl Provider for FailingProvider {
    fn name(&self) -> &str {
        "failing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Err(anyhow!("child provider failed"))
    }
}

struct UsageProvider;
struct ToolCallingChildProvider;
struct DistinctToolCallingChildProvider;
struct ApprovalRouteTool;
struct WorktreeWriteProvider;
struct WorktreeWriteTool;

#[async_trait]
impl Provider for UsageProvider {
    fn name(&self) -> &str {
        "usage"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::Usage(UsageStats {
                prompt_tokens: 8,
                completion_tokens: 5,
                ..UsageStats::default()
            })),
            Ok(ProviderChunk::TextDelta("too expensive".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ToolCallingChildProvider {
    fn name(&self) -> &str {
        "tool-calling-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let tool_result_seen = request
            .messages
            .iter()
            .any(|message| matches!(message.role, sigil_kernel::MessageRole::Tool));
        if tool_result_seen {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("tool route done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }

        let args = r#"{"path":"README.md"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-read-1".to_owned(),
                name: "read_file".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-read-1".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-read-1".to_owned(),
                name: "read_file".to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for DistinctToolCallingChildProvider {
    fn name(&self) -> &str {
        "distinct-tool-calling-child"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool))
        {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("distinct route done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let path = if request.messages.iter().any(|message| {
            message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("read_a"))
        }) {
            "README.md"
        } else {
            "Cargo.toml"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-read-distinct".to_owned(),
                name: "read_file".to_owned(),
                args_json: json!({"path": path}).to_string(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for WorktreeWriteProvider {
    fn name(&self) -> &str {
        "worktree-write"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        provider_capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if request
            .messages
            .iter()
            .any(|message| matches!(message.role, MessageRole::Tool))
        {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "isolated worktree edit complete".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        let args = r#"{"path":"base.txt","content":"isolated edit\n"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-worktree-write".to_owned(),
                name: "write_file".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-worktree-write".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-worktree-write".to_owned(),
                name: "write_file".to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Tool for WorktreeWriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: "Write one test file inside the active workspace.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing worktree test path"))?;
        let content = args
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("missing worktree test content"))?;
        fs::write(ctx.workspace_root.join(path), content)?;
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "written",
            ToolResultMeta::default(),
        ))
    }
}

#[async_trait]
impl Tool for ApprovalRouteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one file for approval route tests.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(ApprovalMode::Ask))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call_id,
            "read_file",
            "read contents",
            ToolResultMeta::default(),
        ))
    }
}

struct ResultReplayProvider;

#[async_trait]
impl Provider for ResultReplayProvider {
    fn name(&self) -> &str {
        "result-replay"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        let mut capabilities = provider_capabilities();
        capabilities.supports_agent_result_replay = true;
        capabilities
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("writer inspected".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

fn root_config() -> RootConfig {
    RootConfig {
        workspace: WorkspaceConfig {
            root: ".".to_owned(),
        },
        storage: Default::default(),
        session: SessionConfig {
            log_dir: Some(".sigil/sessions".to_owned()),
            retention: Default::default(),
        },
        agent: AgentConfig {
            provider: "deepseek".to_owned(),
            model: "deepseek-v4-flash".to_owned(),
            max_turns: Some(12),
            tool_timeout_secs: 45,
        },
        permission: PermissionConfig::default(),
        model_request: Default::default(),
        memory: MemoryConfig { enabled: true },
        skills: Default::default(),
        compaction: CompactionConfig::default(),
        code_intelligence: sigil_kernel::CodeIntelligenceConfig::default(),
        terminal: Default::default(),
        execution: Default::default(),
        verification: Default::default(),
        appearance: Default::default(),
        task: Default::default(),
        providers: BTreeMap::from([(
            "deepseek".to_owned(),
            json!({
                "base_url": "https://example.com",
            }),
        )]),
        web: Default::default(),
        mcp_servers: Vec::new(),
    }
}

fn provider_capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        exact_prefix_cache: true,
        reports_cache_tokens: true,
        reasoning_stream: ReasoningStreamSupport::Native,
        supports_reasoning_effort: true,
        supports_tool_stream: true,
        supports_background_tasks: false,
        supports_response_handles: false,
        supports_reasoning_artifacts: false,
        supports_structured_output: true,
        supports_assistant_prefix_seed: false,
        supports_schema_constrained_tools: true,
        supports_agent_background_resume: false,
        supports_agent_thread_usage: false,
        supports_agent_result_replay: false,
        supports_infill_completion: false,
        supports_system_fingerprint: true,
        tool_name_max_chars: 64,
    }
}

fn provider_capability_hash(capabilities: &ProviderCapabilities) -> Result<String> {
    let bytes = serde_json::to_vec(&serde_json::to_value(capabilities)?)?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn run_options(workspace_root: PathBuf) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root,
        max_turns: Some(4),
        tool_timeout_secs: 30,
        reasoning_effort: None,
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: sigil_kernel::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig { enabled: false },
        compaction_config: CompactionConfig::default(),
    }
}

fn step(id: &str) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(id)?,
        title: format!("run {id}"),
        display_name: Some(id.to_owned()),
        detail: Some("test child step".to_owned()),
        role: AgentRole::SubagentRead,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    })
}

fn write_step(id: &str) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(id)?,
        title: format!("write {id}"),
        display_name: Some(id.to_owned()),
        detail: Some("test write child step".to_owned()),
        role: AgentRole::SubagentWrite,
        depends_on: Vec::new(),
        mode: None,
        isolation: None,
    })
}

fn changeset_step(id: &str) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(id)?,
        title: format!("propose {id}"),
        display_name: Some(id.to_owned()),
        detail: Some("test changeset-only child step".to_owned()),
        role: AgentRole::SubagentWrite,
        depends_on: Vec::new(),
        mode: Some(sigil_kernel::TaskStepMode::Write),
        isolation: Some(sigil_kernel::TaskIsolationMode::ChangesetOnly),
    })
}

fn worktree_step(id: &str) -> Result<TaskStepSpec> {
    Ok(TaskStepSpec {
        step_id: TaskStepId::new(id)?,
        title: format!("isolate {id}"),
        display_name: Some(id.to_owned()),
        detail: Some("test physical worktree child step".to_owned()),
        role: AgentRole::SubagentWrite,
        depends_on: Vec::new(),
        mode: Some(TaskStepMode::Write),
        isolation: Some(TaskIsolationMode::Worktree),
    })
}

fn changeset_output(step_id: &str) -> String {
    json!({
        "change_set": {
            "id": format!("change-{step_id}"),
            "title": format!("Change {step_id}"),
            "summary": format!("Would update {step_id}.txt"),
            "risk": "low",
            "files": [{
                "path": format!("{step_id}.txt"),
                "action": "update",
                "risk": "low",
                "additions": 1,
                "deletions": 0
            }],
            "validations": []
        },
        "artifact": {
            "media_type": "text/x-diff",
            "content": format!("--- /dev/null\n+++ b/{step_id}.txt\n@@\n+{step_id}\n")
        }
    })
    .to_string()
}

fn supervisor_with_budget(budget: AgentBudgetPolicy) -> Result<AgentSupervisor> {
    Ok(AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&root_config())?,
        budget,
        provider_capabilities(),
    ))
}

fn task_role_runner_with_rate_limited_participant(
    supervisor: AgentSupervisor,
    purpose: TaskParticipantPurpose,
    starts: Arc<AtomicUsize>,
) -> AgentSupervisorTaskChildRunner {
    let rate_limited = || {
        Box::new(RateLimitedTaskChildProvider {
            starts: Arc::clone(&starts),
            rate_limits_remaining: Arc::new(AtomicUsize::new(1)),
        }) as Box<dyn Provider>
    };
    let text = |text| Box::new(TextProvider { text }) as Box<dyn Provider>;
    let planner = if purpose == TaskParticipantPurpose::Planner {
        rate_limited()
    } else {
        text("planner done")
    };
    let synthesis = if purpose == TaskParticipantPurpose::Synthesis {
        rate_limited()
    } else {
        text("synthesis done")
    };
    AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor,
        Agent::new(planner, ToolRegistry::new()),
        Agent::new(text("executor done"), ToolRegistry::new()),
        Agent::new(text("reader done"), ToolRegistry::new()),
        Agent::new(text("writer done"), ToolRegistry::new()),
        Agent::new(synthesis, ToolRegistry::new()),
    )
}

#[test]
fn root_budget_allows_one_planner_owned_discovery_level() {
    let budget = AgentBudgetPolicy::from_root_config(&root_config());

    assert_eq!(budget.max_depth, 2);
}

fn agent_route_statuses(session: &Session) -> Vec<sigil_kernel::AgentRouteStatus> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::AgentApprovalRoute(route)) => {
                Some(route.status)
            }
            _ => None,
        })
        .collect()
}

fn agent_approval_routes(session: &Session) -> Vec<&sigil_kernel::AgentApprovalRouteEntry> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::AgentApprovalRoute(route)) => {
                Some(route)
            }
            _ => None,
        })
        .collect()
}

fn task_route_statuses(session: &Session) -> Vec<TaskRouteStatus> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskSubagentApprovalRoute(
                TaskSubagentApprovalRouteEntry { status, .. },
            )) => Some(*status),
            _ => None,
        })
        .collect()
}

fn task_approval_routes(session: &Session) -> Vec<&TaskSubagentApprovalRouteEntry> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskSubagentApprovalRoute(
                route,
            )) => Some(route),
            _ => None,
        })
        .collect()
}

fn child_start(step: TaskStepSpec, workspace_root: PathBuf) -> Result<AgentTaskChildStart> {
    let task_id = TaskId::new("task_1")?;
    let child_task_id = TaskId::new(format!("child_v1_{}", step.step_id.as_str()))?;
    let child_session_ref = child_session_ref(&task_id, &step.step_id, &child_task_id)?;
    Ok(AgentTaskChildStart {
        task_id,
        parent_thread_id: AgentThreadId::new("main")?,
        parent_depth: 0,
        batch_id: None,
        batch_member_key: None,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        plan_version: 1,
        step,
        child_task_id,
        child_session_ref,
        child_input: AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
            "inspect code",
        )]),
        objective: "inspect code".to_owned(),
        workspace_root,
        provider_capabilities: provider_capabilities(),
        role: AgentRole::SubagentRead,
        invocation_mode: AgentInvocationMode::Foreground,
        invocation_source: AgentInvocationSource::Task,
        isolated_workspace_id: None,
    })
}

fn chat_child_start(profile_id: &str, workspace_root: PathBuf) -> Result<AgentChatChildStart> {
    let call_id = format!("call_{profile_id}");
    let profile_id = sigil_kernel::AgentProfileId::new(profile_id)?;
    let thread_id = super::chat_agent_thread_id_for_call(&call_id, &profile_id)?;
    let cancellation = RunCancellationOwner::new().handle();
    let authority = DelegationAuthority::ModelProactive;
    let grant = AgentInvocationGrant::mint(
        AgentInvocationGrantBinding {
            source: AgentInvocationGrantSource::Conversation {
                source_turn: sigil_kernel::ConversationTurnRef::new(
                    "test-session",
                    "test-message",
                    "test-run",
                )?,
            },
            authority: authority.clone(),
            root_logical_run_id: "test-run".to_owned(),
            profile_id: profile_id.clone(),
            role: AgentRole::SubagentRead,
            isolation: TaskIsolationMode::SharedReadOnly,
            permission_upper_bound: PermissionConfig {
                mode: sigil_kernel::PermissionMode::ReadOnly,
                ..PermissionConfig::default()
            },
            network_upper_bound: NetworkPolicy::Deny,
            tool_contract_fingerprint: "sha256:test-contracts".to_owned(),
            workspace_snapshot_id: sigil_kernel::agent_invocation_workspace_snapshot_id(
                &workspace_root,
            )?,
            root_cancellation_scope_id: cancellation.scope_id().to_owned(),
            expires_at_ms: u64::MAX,
        },
        1,
    )?;
    Ok(AgentChatChildStart {
        call_id,
        budget_scope_id: TaskId::new("chat_1")?,
        parent_thread_id: AgentThreadId::new("main")?,
        parent_depth: 0,
        batch_id: None,
        batch_member_key: None,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        profile_id: profile_id.clone(),
        role: AgentRole::SubagentRead,
        child_session_ref: SessionRef::new_relative(format!(
            "children/{}.jsonl",
            profile_id.as_str()
        ))?,
        objective: "inspect code".to_owned(),
        prompt: "inspect code".to_owned(),
        workspace_root,
        provider_capabilities: provider_capabilities(),
        invocation_mode: AgentInvocationMode::JoinBeforeFinal,
        invocation_source: AgentInvocationSource::Chat,
        invocation_grant: grant.clone(),
        delegation_admission: AgentDelegationAdmissionEntry {
            thread_id,
            profile_id,
            invocation_mode: AgentInvocationMode::JoinBeforeFinal,
            invocation_source: AgentInvocationSource::Chat,
            authority: DelegationAuthorityRecord::ModelProactive,
            objective_hash: super::hash_text("inspect code"),
            tool_contract_fingerprint: "sha256:test-contracts".to_owned(),
            invocation_grant: Some(grant.durable_record()?),
            admitted_at_ms: None,
        },
        display_name_hint: Some("inspect".to_owned()),
    })
}

fn rebind_chat_delegation_admission(start: &mut AgentChatChildStart) -> Result<()> {
    start.delegation_admission.thread_id =
        super::chat_agent_thread_id_for_call(&start.call_id, &start.profile_id)?;
    start.delegation_admission.profile_id = start.profile_id.clone();
    start.delegation_admission.invocation_mode = start.invocation_mode;
    start.delegation_admission.invocation_source = start.invocation_source;
    start.delegation_admission.objective_hash =
        super::hash_text(&sigil_kernel::safe_persistence_text(&start.objective));
    let authority = match &start.delegation_admission.authority {
        DelegationAuthorityRecord::UserExplicit => DelegationAuthority::UserExplicit,
        DelegationAuthorityRecord::AcceptedTaskPlan {
            task_id,
            plan_version,
            step_id,
        } => DelegationAuthority::AcceptedTaskPlan {
            task_id: task_id.clone(),
            plan_version: *plan_version,
            step_id: step_id.clone(),
        },
        DelegationAuthorityRecord::ModelProactive => DelegationAuthority::ModelProactive,
        DelegationAuthorityRecord::SystemRecovery => DelegationAuthority::SystemRecovery,
    };
    let cancellation = RunCancellationOwner::new().handle();
    let grant = AgentInvocationGrant::mint(
        AgentInvocationGrantBinding {
            source: AgentInvocationGrantSource::Conversation {
                source_turn: sigil_kernel::ConversationTurnRef::new(
                    "test-session",
                    "test-message",
                    "test-run",
                )?,
            },
            authority,
            root_logical_run_id: "test-run".to_owned(),
            profile_id: start.profile_id.clone(),
            role: start.role,
            isolation: if start.role == AgentRole::SubagentWrite {
                TaskIsolationMode::ChangesetOnly
            } else {
                TaskIsolationMode::SharedReadOnly
            },
            permission_upper_bound: PermissionConfig::default(),
            network_upper_bound: NetworkPolicy::Deny,
            tool_contract_fingerprint: start.delegation_admission.tool_contract_fingerprint.clone(),
            workspace_snapshot_id: sigil_kernel::agent_invocation_workspace_snapshot_id(
                &start.workspace_root,
            )?,
            root_cancellation_scope_id: cancellation.scope_id().to_owned(),
            expires_at_ms: u64::MAX,
        },
        1,
    )?;
    start.delegation_admission.invocation_grant = Some(grant.durable_record()?);
    start.invocation_grant = grant;
    Ok(())
}

#[test]
fn supervisor_captures_profile_snapshot_before_spawn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("inspect")?, temp.path().to_path_buf())?,
    )?;

    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("thread was projected");
    assert_eq!(
        projected.profile_id.as_ref().map(|id| id.as_str()),
        Some(EXPLORE_PROFILE_ID)
    );
    assert!(!projection.profiles.is_empty());
    assert!(projected.run_context.is_some());
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::Control(sigil_kernel::ControlEntry::AgentProfileCaptured(_))
        )
    }));
    Ok(())
}

#[test]
fn supervisor_rejects_incomplete_batch_identity_before_control_append() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(step("inspect")?, temp.path().to_path_buf())?;
    start.batch_id = Some(sigil_kernel::AgentBatchId::new("batch_incomplete")?);

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("incomplete batch identity should be rejected");

    assert!(
        error
            .to_string()
            .contains("requires both batch id and member key")
    );
    assert!(session.entries().is_empty());
    assert!(handler.events.is_empty());
    Ok(())
}

#[test]
fn chat_child_start_projects_sensitive_objective_and_prompt_hash_before_control_append()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let raw = "inspect https://example.com/private?signature=thread-start-secret exactly";
    let mut start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    start.objective = raw.to_owned();
    start.prompt = raw.to_owned();
    rebind_chat_delegation_admission(&mut start)?;

    let thread = supervisor.begin_chat_child_thread(&mut session, &mut handler, start)?;

    let durable = serde_json::to_string(session.entries())?;
    assert!(!durable.contains("thread-start-secret"));
    assert!(!durable.contains(raw));
    let projected = session
        .agent_thread_state_projection()
        .threads
        .get(&thread.thread_id)
        .cloned()
        .expect("thread should project");
    let safe = sigil_kernel::safe_persistence_text(raw);
    assert_eq!(projected.objective, safe);
    assert_eq!(projected.prompt_hash, super::hash_text(&safe));
    assert_ne!(projected.prompt_hash, super::hash_text(raw));
    Ok(())
}

#[test]
fn chat_child_start_rejects_invalid_disabled_and_model_invisible_profiles() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_chat_child_thread(
            &mut session,
            &mut handler,
            chat_child_start("missing", temp.path().to_path_buf())?,
        )
        .expect_err("missing profile rejected");
    assert!(error.to_string().contains("not registered"));

    let error = supervisor
        .begin_chat_child_thread(
            &mut session,
            &mut handler,
            chat_child_start("plan", temp.path().to_path_buf())?,
        )
        .expect_err("model-invisible profile rejected");
    assert!(error.to_string().contains("not model-invocable"));

    let mut disabled_config = root_config();
    disabled_config.task.enabled = false;
    let disabled_supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&disabled_config)?,
        AgentBudgetPolicy::from_root_config(&disabled_config),
        provider_capabilities(),
    );
    let error = disabled_supervisor
        .begin_chat_child_thread(
            &mut session,
            &mut handler,
            chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
        )
        .expect_err("disabled profile rejected");
    assert!(error.to_string().contains("is disabled"));
    Ok(())
}

#[test]
fn chat_child_start_rejects_mention_when_profile_is_not_user_invocable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    let agent_dir = workspace.join(".sigil").join("agents").join("model-only");
    fs::create_dir_all(&agent_dir)?;
    fs::write(
        agent_dir.join("agent.toml"),
        r#"
description = "Model-only helper."
instructions = "Only the model may invoke this profile."
trust = "trusted"
invocation_policy = "model_allowed"
user_invocable = false
model_invocable = true
"#,
    )?;

    let registry =
        AgentProfileRegistry::from_root_config_with_workspace(&root_config(), &workspace)?;
    let supervisor = AgentSupervisor::new(
        registry,
        AgentBudgetPolicy::from_root_config(&root_config()),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = chat_child_start("model-only", workspace)?;
    start.invocation_source = AgentInvocationSource::Mention;

    let error = supervisor
        .begin_chat_child_thread(&mut session, &mut handler, start)
        .expect_err("manual mention rejects non-user-invocable profile");

    assert!(error.to_string().contains("not user-invocable"));
    Ok(())
}

#[test]
fn chat_child_start_rejects_write_capable_profile_without_lease_support() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.subagent_read.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["write_file".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    rebind_chat_delegation_admission(&mut start)?;

    let error = supervisor
        .begin_chat_child_thread(&mut session, &mut handler, start)
        .expect_err("write-capable chat profile rejected");

    assert!(
        error
            .to_string()
            .contains("write-capable agent requires guarded changeset-only scope")
    );
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Failed
            && thread
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("write-capable agents require"))
    }));
    Ok(())
}

#[test]
fn record_chat_child_failure_appends_failed_status_and_releases_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_chat_child_thread(
        &mut session,
        &mut handler,
        chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);

    supervisor.record_chat_child_failure(
        &mut session,
        &mut handler,
        &thread,
        "child failed".to_owned(),
    )?;

    assert!(supervisor.active_profile_ids().is_empty());
    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("chat thread projected");
    assert_eq!(projected.status, sigil_kernel::AgentThreadStatus::Failed);
    assert_eq!(projected.reason.as_deref(), Some("child failed"));
    Ok(())
}

#[test]
fn record_chat_child_result_persists_final_answer_ref_and_releases_budget() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_chat_child_thread(
        &mut session,
        &mut handler,
        chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
    )?;
    let final_answer_ref = AgentFinalAnswerRef {
        session_ref: thread.child_session_ref.clone(),
        message_id: "msg-child-final".to_owned(),
        content_hash: "sha256:child-final".to_owned(),
        char_count: "child done".chars().count(),
    };

    let handler_dyn: &mut (dyn EventHandler + Send) = &mut handler;
    supervisor.record_chat_child_result(
        &mut session,
        handler_dyn,
        &thread,
        TaskChildSessionStatus::Completed,
        &AgentResultMaterialization::inline("child done", Some(final_answer_ref.clone())),
        &AgentRunOutcome::default(),
        None,
    )?;

    assert!(supervisor.active_profile_ids().is_empty());
    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("chat thread projected");
    assert_eq!(projected.status, sigil_kernel::AgentThreadStatus::Completed);
    assert_eq!(
        projected
            .result
            .as_ref()
            .and_then(|result| result.final_answer_ref.as_ref()),
        Some(&final_answer_ref)
    );
    Ok(())
}

#[test]
fn send_agent_message_reports_inactive_thread_and_missing_mailbox() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;

    let inactive_error = supervisor
        .send_agent_message(
            &AgentThreadId::new("missing")?,
            AgentMailboxMessage {
                route_id: AgentRouteId::new("route_missing")?,
                prompt: "follow up".to_owned(),
            },
        )
        .expect_err("inactive thread rejects mailbox message");
    assert_eq!(inactive_error, "agent thread is not active");

    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let foreground = supervisor.begin_chat_child_thread(
        &mut session,
        &mut handler,
        chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?,
    )?;

    let missing_mailbox_error = supervisor
        .send_agent_message(
            &foreground.thread_id,
            AgentMailboxMessage {
                route_id: AgentRouteId::new("route_foreground")?,
                prompt: "follow up".to_owned(),
            },
        )
        .expect_err("foreground child has no active mailbox");
    assert_eq!(missing_mailbox_error, "agent thread has no active mailbox");

    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.call_id = "call_background_mailbox".to_owned();
    background_start.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut background_start)?;
    let mut background =
        supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;
    let route_id = AgentRouteId::new("route_background")?;
    supervisor
        .send_agent_message(
            &background.thread_id,
            AgentMailboxMessage {
                route_id: route_id.clone(),
                prompt: "continue".to_owned(),
            },
        )
        .map_err(|error| anyhow!(error))?;

    let received = background
        .mailbox_rx
        .as_mut()
        .expect("background child should have mailbox")
        .try_recv()
        .expect("message should be queued");
    assert_eq!(received.route_id, route_id);
    assert_eq!(received.prompt, "continue");
    Ok(())
}

#[tokio::test]
async fn route_agent_message_records_mailbox_delivery_state() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut background_start)?;
    let background =
        supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;
    let mut runtime = AgentToolRuntime::new(supervisor, root_config(), ToolRegistry::new());

    let (_result, controls) = runtime
        .route_agent_message(
            &mut session,
            background.thread_id.clone(),
            "continue".to_owned(),
            &run_options(temp.path().to_path_buf()),
        )
        .await?;

    let mailbox_statuses = controls
        .iter()
        .filter_map(|control| match control {
            sigil_kernel::ControlEntry::AgentMailboxMessage(entry) => Some(entry.status),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        mailbox_statuses,
        vec![
            sigil_kernel::AgentMailboxStatus::Queued,
            sigil_kernel::AgentMailboxStatus::Delivered
        ]
    );
    let projection = session.agent_thread_state_projection();
    let mailbox = projection
        .mailbox_messages
        .values()
        .next()
        .expect("mailbox message should be projected");
    assert_eq!(mailbox.status, sigil_kernel::AgentMailboxStatus::Delivered);
    Ok(())
}

#[test]
fn foreground_background_request_reports_missing_foreground() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let no_foreground = supervisor
        .request_foreground_background()
        .expect_err("missing foreground child should reject background request");
    assert_eq!(
        no_foreground,
        "no foreground child agent is currently running"
    );

    let mut background_start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    background_start.call_id = "call_background_budget".to_owned();
    background_start.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut background_start)?;
    supervisor.begin_chat_child_thread(&mut session, &mut handler, background_start)?;

    let missing_foreground = supervisor
        .request_foreground_background()
        .expect_err("background-only state has no foreground child to move");
    assert_eq!(
        missing_foreground,
        "no foreground child agent is currently running"
    );
    Ok(())
}

#[test]
fn supervisor_enforces_max_depth() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_depth = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("inspect")?, temp.path().to_path_buf())?,
        )
        .expect_err("max_depth=0 denies child thread");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("max_depth=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_nested_depth_from_parent_thread() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_depth = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(step("nested")?, temp.path().to_path_buf())?;
    start.parent_thread_id = AgentThreadId::new("child_parent")?;
    start.parent_depth = 1;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("nested child is denied at max_depth=1");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("max_depth=1"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_max_subagents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("two")?, temp.path().to_path_buf())?,
        )
        .expect_err("max_subagents denies second active child");

    assert!(error.to_string().contains("agent budget denied"));
    assert!(
        error
            .to_string()
            .contains("agent thread budget exceeded: [task].max_subagents=1")
    );
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn task_batch_reservation_is_atomic_and_claimed_by_child_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = child_start(step("task_batch_first")?, temp.path().to_path_buf())?;
    first.invocation_mode = AgentInvocationMode::JoinBeforeFinal;
    let mut second = child_start(step("task_batch_second")?, temp.path().to_path_buf())?;
    second.invocation_mode = AgentInvocationMode::JoinBeforeFinal;
    let starts = vec![first, second];

    let reservation = supervisor.reserve_task_child_batch(&starts)?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let threads = starts
        .into_iter()
        .map(|start| supervisor.begin_task_child_thread(&mut session, &mut handler, start))
        .collect::<Result<Vec<_>>>()?;
    reservation.commit();

    assert_eq!(threads.len(), 2);
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    for thread in threads {
        supervisor.record_task_child_failure(
            &mut session,
            &mut handler,
            &thread,
            "test cleanup".to_owned(),
        )?;
    }
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn dropped_task_batch_reservation_releases_every_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut first = child_start(step("task_batch_drop_first")?, temp.path().to_path_buf())?;
    first.invocation_mode = AgentInvocationMode::JoinBeforeFinal;
    let mut second = child_start(step("task_batch_drop_second")?, temp.path().to_path_buf())?;
    second.invocation_mode = AgentInvocationMode::JoinBeforeFinal;

    let reservation = supervisor.reserve_task_child_batch(&[first, second])?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    drop(reservation);

    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_is_atomic_and_claimed_by_child_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_batch_first".to_owned();
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_batch_second".to_owned();
    rebind_chat_delegation_admission(&mut second)?;
    let starts = vec![first, second];

    let reservation = supervisor.reserve_chat_child_batch(&starts)?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let threads = starts
        .into_iter()
        .map(|start| supervisor.begin_chat_child_thread(&mut session, &mut handler, start))
        .collect::<Result<Vec<_>>>()?;
    reservation.commit();

    assert_eq!(threads.len(), 2);
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    for thread in threads {
        supervisor.record_chat_child_failure(
            &mut session,
            &mut handler,
            &thread,
            "test cleanup".to_owned(),
        )?;
    }
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn background_chat_batch_reservation_is_atomic_and_creates_mailboxes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_background_batch_first".to_owned();
    first.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_background_batch_second".to_owned();
    second.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut second)?;
    let starts = vec![first, second];

    let reservation = supervisor.reserve_chat_child_batch(&starts)?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let threads = starts
        .into_iter()
        .map(|start| supervisor.begin_chat_child_thread(&mut session, &mut handler, start))
        .collect::<Result<Vec<_>>>()?;
    reservation.commit();

    assert!(threads.iter().all(|thread| thread.mailbox_rx.is_some()));
    for thread in threads {
        supervisor.record_chat_child_failure(
            &mut session,
            &mut handler,
            &thread,
            "test cleanup".to_owned(),
        )?;
    }
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_rejects_mixed_invocation_modes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_background_batch_second".to_owned();
    second.invocation_mode = AgentInvocationMode::Background;
    rebind_chat_delegation_admission(&mut second)?;

    let error = supervisor
        .reserve_chat_child_batch(&[first, second])
        .err()
        .expect("mixed invocation modes should be rejected");

    assert!(error.to_string().contains("cannot mix invocation modes"));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn dropped_chat_batch_reservation_releases_every_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_batch_drop_first".to_owned();
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_batch_drop_second".to_owned();
    rebind_chat_delegation_admission(&mut second)?;

    let reservation = supervisor.reserve_chat_child_batch(&[first, second])?;
    assert_eq!(supervisor.active_profile_ids().len(), 2);
    drop(reservation);

    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_rejects_capacity_without_partial_slots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut first = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    first.call_id = "call_batch_first".to_owned();
    rebind_chat_delegation_admission(&mut first)?;
    let mut second = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;
    second.call_id = "call_batch_second".to_owned();
    rebind_chat_delegation_admission(&mut second)?;

    let error = supervisor
        .reserve_chat_child_batch(&[first, second])
        .err()
        .expect("oversized batch should be rejected");

    assert!(error.to_string().contains("requested=2"));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn chat_batch_reservation_rejects_duplicate_identity_without_partial_slots() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let start = chat_child_start(EXPLORE_PROFILE_ID, temp.path().to_path_buf())?;

    let error = supervisor
        .reserve_chat_child_batch(&[start.clone(), start])
        .err()
        .expect("duplicate batch identity should be rejected");

    assert!(error.to_string().contains("duplicate thread"));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[test]
fn release_allows_next_spawn_after_max_subagents_slot_opens() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let first = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    supervisor.record_task_child_result(
        &mut session,
        &mut handler,
        &first,
        SessionRef::new_relative("children/task_1/one.jsonl")?,
        TaskChildSessionStatus::Completed,
        &AgentResultMaterialization::inline("one done", None),
        &AgentRunOutcome::default(),
        None,
    )?;
    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("two")?, temp.path().to_path_buf())?,
    )?;

    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn cancel_foreground_run_releases_active_child_and_appends_audit() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 2;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let first = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("one")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);

    let impact = supervisor.cancel_foreground_run();
    assert_eq!(impact.foreground_children_interrupted.len(), 1);
    assert_eq!(
        impact.foreground_children_interrupted[0].thread_id,
        first.thread_id
    );
    assert!(supervisor.active_profile_ids().is_empty());

    AgentSupervisor::append_foreground_cancel_audit(
        &mut session,
        &mut handler,
        impact,
        "run cancelled from test",
    )?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .get(&first.thread_id)
        .expect("cancelled thread projected");
    assert_eq!(thread.status, sigil_kernel::AgentThreadStatus::Interrupted);
    assert_eq!(thread.reason.as_deref(), Some("run cancelled from test"));

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("two")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[test]
fn budget_policy_from_config_exposes_max_subagents_and_accessors() -> Result<()> {
    let mut config = root_config();
    config.task.max_subagents = 3;
    let budget = AgentBudgetPolicy::from_root_config(&config);

    assert_eq!(budget.max_subagents, 3);

    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    assert_eq!(supervisor.budget().max_subagents, 3);
    assert_eq!(supervisor.registry().profiles().len(), 4);

    let mut default_config = root_config();
    default_config.task.max_subagents = 4;
    let default_budget = AgentBudgetPolicy::from_root_config(&default_config);
    assert_eq!(default_budget.max_subagents, 4);

    Ok(())
}

#[test]
fn budget_policy_uses_default_limits_when_config_values_are_omitted() {
    let config = root_config();

    let default_budget = AgentBudgetPolicy::from_root_config(std::hint::black_box(&config));

    assert_eq!(default_budget.max_subagents, 8);
}

#[test]
fn supervisor_enforces_max_subagents_for_background_read_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(step("background")?, temp.path().to_path_buf())?;
    start.invocation_mode = AgentInvocationMode::Background;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("background budget denies read child");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[task].max_subagents=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_max_subagents_for_readonly_child() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let error = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("readonly")?, temp.path().to_path_buf())?,
        )
        .expect_err("readonly budget denies read child");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[task].max_subagents=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_enforces_max_subagents_for_readonly_scoped_writer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let mut budget = AgentBudgetPolicy::from_root_config(&config);
    budget.max_subagents = 0;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        budget,
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("write_budget")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("write budget denies write child");

    assert!(error.to_string().contains("agent budget denied"));
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("[task].max_subagents=0"))
    }));
    Ok(())
}

#[test]
fn supervisor_denies_background_worker_even_when_scope_is_readonly() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("background_write")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.invocation_mode = AgentInvocationMode::Background;

    let error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("background worker still requires isolated merge support");

    assert!(
        error
            .to_string()
            .contains("background write-capable agent requires isolated merge support")
    );
    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Failed
            && thread.reason.as_deref().is_some_and(|reason| {
                reason.contains("background write-capable agents require isolated merge support")
            })
    }));
    Ok(())
}

#[test]
fn supervisor_denied_budget_appends_control_entry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 0;
    let supervisor = supervisor_with_budget(budget)?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();

    let _ = supervisor
        .begin_task_child_thread(
            &mut session,
            &mut handler,
            child_start(step("inspect")?, temp.path().to_path_buf())?,
        )
        .expect_err("thread budget denies child");

    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::AgentThreadStatusChanged(status)
            ) if status.status == sigil_kernel::AgentThreadStatus::Failed
        )
    }));
    Ok(())
}

#[test]
fn task_child_status_and_terminal_status_cover_edges() {
    let mut max_turns = AgentRunOutcome {
        terminal_reason: AgentRunTerminalReason::MaxTurns,
        ..AgentRunOutcome::default()
    };
    assert_eq!(
        task_child_status_from_outcome("partial", &max_turns),
        TaskChildSessionStatus::Interrupted
    );

    max_turns.terminal_reason = AgentRunTerminalReason::FinalAnswer;
    max_turns.approval_denials = 1;
    assert_eq!(
        task_child_status_from_outcome("denied", &max_turns),
        TaskChildSessionStatus::Failed
    );

    assert_eq!(
        task_child_status_from_outcome(
            "",
            &AgentRunOutcome {
                tool_errors: vec![ToolError {
                    kind: ToolErrorKind::Internal,
                    message: "boom".to_owned(),
                    retryable: false,
                    details: Value::Null,
                }],
                ..AgentRunOutcome::default()
            }
        ),
        TaskChildSessionStatus::Failed
    );

    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Started),
        AgentThreadTerminalStatus::Interrupted
    );
    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Interrupted),
        AgentThreadTerminalStatus::Interrupted
    );
    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Cancelled),
        AgentThreadTerminalStatus::Cancelled
    );
    assert_eq!(
        agent_terminal_status_from_task_child(TaskChildSessionStatus::Unavailable),
        AgentThreadTerminalStatus::Failed
    );
}

#[test]
fn supervisor_denies_background_write_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("edit")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.invocation_mode = AgentInvocationMode::Background;

    let _error = supervisor
        .begin_task_child_thread(&mut session, &mut handler, start)
        .expect_err("background write agent is denied");

    let projection = session.agent_thread_state_projection();
    assert!(projection.threads.values().any(|thread| {
        thread.reason.as_deref().is_some_and(|reason| {
            reason.contains("background write-capable agents require isolated merge support")
        })
    }));
    Ok(())
}

#[test]
fn supervisor_worker_scope_ignores_unguarded_mcp_write_prefix_config() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: Vec::new(),
        prefixes: vec!["mcp__filesystem__".to_owned()],
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("mcp_write")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    assert!(
        thread.thread_id.as_str().starts_with("agent_v1_"),
        "worker should keep its guarded builtin scope instead of inheriting unguarded role config"
    );
    let profile = supervisor
        .registry()
        .get(&sigil_kernel::AgentProfileId::new("worker")?)
        .expect("worker profile exists");
    assert!(
        !profile
            .profile
            .tool_scope
            .prefixes
            .iter()
            .any(|prefix| prefix == "mcp__filesystem__")
    );
    Ok(())
}

#[test]
fn supervisor_allows_default_worker_changeset_only_foreground() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("edit")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    assert!(thread.thread_id.as_str().starts_with("agent_v1_"));
    Ok(())
}

#[test]
fn supervisor_worker_scope_ignores_apply_changeset_config_widening() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["apply_changeset".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("changeset")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.step.mode = Some(sigil_kernel::TaskStepMode::Write);
    start.step.isolation = Some(sigil_kernel::TaskIsolationMode::ChangesetOnly);

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    assert!(thread.thread_id.as_str().starts_with("agent_v1_"));
    let profile = supervisor
        .registry()
        .get(&sigil_kernel::AgentProfileId::new("worker")?)
        .expect("worker profile exists");
    assert!(!profile.profile.tool_scope.names.contains("apply_changeset"));
    Ok(())
}

#[test]
fn supervisor_allows_changeset_only_scoped_write_agents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    let scope = sigil_kernel::changeset_only_child_tool_scope();
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: scope.allow_all,
        names: scope.names.into_iter().collect(),
        prefixes: scope.prefixes,
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut start = child_start(write_step("changeset")?, temp.path().to_path_buf())?;
    start.role = AgentRole::SubagentWrite;
    start.step.mode = Some(sigil_kernel::TaskStepMode::Write);
    start.step.isolation = Some(sigil_kernel::TaskIsolationMode::ChangesetOnly);

    let thread = supervisor.begin_task_child_thread(&mut session, &mut handler, start)?;

    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("thread should be projected");
    assert_eq!(projected.status, sigil_kernel::AgentThreadStatus::Running);
    assert_eq!(projected.reason.as_deref(), Some("child session started"));
    Ok(())
}

#[test]
fn supervisor_records_changed_paths_and_usage_in_agent_result() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let thread = supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("inspect")?, temp.path().to_path_buf())?,
    )?;
    let mut outcome = AgentRunOutcome::default();
    outcome
        .changed_files
        .push("crates/sigil-runtime/src/lib.rs".to_owned());
    let usage = AgentUsageSummary {
        input_tokens: 8,
        output_tokens: 5,
        total_tokens: 13,
        cached_tokens: Some(2),
    };

    supervisor.record_task_child_result(
        &mut session,
        &mut handler,
        &thread,
        SessionRef::new_relative("children/task_1/inspect.jsonl")?,
        sigil_kernel::TaskChildSessionStatus::Completed,
        &AgentResultMaterialization::inline("done", None),
        &outcome,
        Some(usage.clone()),
    )?;

    let projection = session.agent_thread_state_projection();
    let projected = projection
        .threads
        .get(&thread.thread_id)
        .expect("thread was projected");
    let result = projected.result.as_ref().expect("result was recorded");
    assert_eq!(result.changed_paths, outcome.changed_files);
    assert_eq!(result.usage.as_ref(), Some(&usage));
    assert!(
        projected
            .merge_safe_points
            .iter()
            .any(|safe_point| safe_point.parent_thread_id.as_str() == "main")
    );
    Ok(())
}

#[test]
fn write_capable_scope_detects_specific_mcp_prefixes() {
    let mcp_scope = ToolRegistryScope {
        prefixes: vec!["mcp__gitlab__".to_owned()],
        ..ToolRegistryScope::default()
    };
    assert!(tool_scope_is_write_capable(&mcp_scope));

    let read_scope = ToolRegistryScope {
        names: BTreeSet::from(["grep".to_owned()]),
        ..ToolRegistryScope::default()
    };
    assert!(!tool_scope_is_write_capable(&read_scope));
}

#[test]
fn cancel_foreground_does_not_cancel_background_child() -> Result<()> {
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;

    let impact = supervisor.cancel_foreground_run();

    assert_eq!(impact.background_children_cancelled, 0);
    Ok(())
}

#[test]
fn provider_background_resume_defaults_to_interrupted() -> Result<()> {
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;

    assert!(!supervisor.supports_background_resume());
    Ok(())
}

#[test]
fn planner_runtime_tool_view_exposes_only_bounded_discovery() {
    let mut base = ToolRegistry::new();
    base.register(Arc::new(ApprovalRouteTool));

    let without_discovery = planner_tools_with_discovery(&base, 0);
    assert!(without_discovery.specs().is_empty());

    let with_discovery = planner_tools_with_discovery(&base, 2);
    assert_eq!(with_discovery.specs().len(), 1);
    assert!(
        with_discovery
            .spec_for(REQUEST_TASK_DISCOVERY_TOOL_NAME)
            .is_some()
    );
    let discovery = with_discovery
        .spec_for(REQUEST_TASK_DISCOVERY_TOOL_NAME)
        .expect("planner discovery tool");
    assert!(
        discovery
            .description
            .contains("rather than an explicit workspace-relative path")
    );
    assert!(discovery.description.contains("never guess paths"));
    assert!(with_discovery.spec_for("read_file").is_none());
    assert!(base.spec_for("read_file").is_some());
    assert!(
        base.spec_for(REQUEST_TASK_DISCOVERY_TOOL_NAME).is_none(),
        "temporary planner tools must not mutate the role registry"
    );
}

#[tokio::test]
async fn planner_worktree_capability_requires_interactive_git_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let non_git = temp.path().join("non-git");
    fs::create_dir(&non_git)?;
    let git = temp.path().join("git");
    initialize_worktree_test_repository(&git)?;
    let runner = task_role_runner_with_rate_limited_participant(
        supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?,
        TaskParticipantPurpose::Step,
        Arc::new(AtomicUsize::new(0)),
    );

    assert_eq!(
        runner
            .planner_worktree_availability(&run_options(non_git))
            .await,
        TaskPlannerWorktreeAvailability::UnavailableWorkspace
    );

    let mut headless = run_options(git.clone());
    headless.interaction_mode = InteractionMode::Headless;
    assert_eq!(
        runner.planner_worktree_availability(&headless).await,
        TaskPlannerWorktreeAvailability::UnavailableHeadless
    );
    assert_eq!(
        runner
            .planner_worktree_availability(&run_options(git))
            .await,
        TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview
    );
    Ok(())
}

#[tokio::test]
async fn planner_postprocess_failure_marks_thread_failed_and_releases_slot() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(TextProvider {
                text: "planner returned prose without committing a plan",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "reader done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    );
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_planner_postprocess")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "produce a durable task plan".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("plan the task"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await
        .expect_err("planner prose without task_plan_update must fail postprocessing");

    assert!(
        error
            .to_string()
            .contains("did not produce an accepted plan")
    );
    let projection = session.agent_thread_state_projection();
    let failed = projection
        .latest_thread()
        .expect("planner thread is projected");
    assert_eq!(failed.status, sigil_kernel::AgentThreadStatus::Failed);
    assert!(
        failed
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("did not produce an accepted plan"))
    );
    assert!(supervisor.active_profile_ids().is_empty());

    supervisor.begin_task_child_thread(
        &mut session,
        &mut handler,
        child_start(step("slot-reused")?, temp.path().to_path_buf())?,
    )?;
    assert_eq!(supervisor.active_profile_ids().len(), 1);
    Ok(())
}

#[tokio::test]
async fn planner_output_returns_model_owned_task_guidance_decision() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor,
        Agent::new(Box::new(GuidanceApplyPlannerProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "reader done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    );
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_guidance_runtime")?;
    let step_id = TaskStepId::new("step_1")?;
    let accepted_plan = TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: step_id.clone(),
            title: "Inspect runtime guidance".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    };
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "review task guidance".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("review this guidance"),
                ])
                .with_task_plan_update(TaskPlanUpdateContext {
                    task_id: task_id.clone(),
                    max_plan_steps: 4,
                    max_plan_versions: 2,
                    worktree_availability:
                        TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
                })
                .with_task_guidance_assessment(TaskGuidanceAssessmentContext {
                    queue_id: ConversationInputQueueId::new("queue_runtime_guidance")?,
                    task_id,
                    plan_version: 1,
                    dispatch_run_id: "dispatch_runtime_guidance".to_owned(),
                    accepted_plan,
                    eligible_pending_step_ids: vec![step_id],
                }),
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    let applied = output
        .guidance_applied
        .expect("runtime planner output should carry the model guidance decision");
    assert_eq!(applied.queue_id.as_str(), "queue_runtime_guidance");
    assert_eq!(applied.plan_version, 1);
    assert_eq!(applied.target_step_ids[0].as_str(), "step_1");
    assert_eq!(output.accepted_plan.plan_version, 1);
    Ok(())
}

#[tokio::test]
async fn planner_discovery_runs_bounded_probes_in_parallel_and_resumes_without_polling()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 4;
    let supervisor = supervisor_with_budget(budget)?;
    let observed_results = Arc::new(Mutex::new(None));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let mut explore_tools = ToolRegistry::new();
    explore_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(PlannerDiscoveryProvider {
                observed_results: Arc::clone(&observed_results),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(ParallelDiscoveryProvider {
                barrier: Arc::new(tokio::sync::Barrier::new(2)),
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
            }),
            explore_tools,
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    )
    .with_planner_discovery_policy(MultiAgentMode::ExplicitRequestOnly, 3);
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_planner_discovery")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let cancellation = RunCancellationOwner::new();
    let planner_input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("plan the task")])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: task_id.clone(),
                max_plan_steps: 12,
                max_plan_versions: 3,
                worktree_availability:
                    TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
            })
            .with_cancellation(cancellation.handle());
    let mut handler = RecordingEventHandler::default();
    let mut approval = CountingApprovalHandler::default();

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        runner.run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "inspect kernel and runtime before implementation".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: planner_input,
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        ),
    )
    .await
    .expect("planner discovery should complete without polling")?;

    assert_eq!(output.accepted_plan.plan_version, 1);
    assert_eq!(
        approval.decisions, 0,
        "bounded read-only planner discovery must not request headless execution approval"
    );
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    let results = observed_results
        .lock()
        .expect("planner discovery observation lock should not be poisoned")
        .clone()
        .expect("planner should receive discovery results");
    let result_envelope: Value = serde_json::from_str(&results)?;
    let results: Value = serde_json::from_str(
        result_envelope["content"]
            .as_str()
            .expect("planner discovery result content should be a string"),
    )?;
    assert_eq!(results["type"], "task_discovery_results");
    assert!(
        results["batch_id"]
            .as_str()
            .is_some_and(|batch_id| batch_id.starts_with("discovery_"))
    );
    assert_eq!(results["members"][0]["probe_id"], "kernel");
    assert_eq!(results["members"][1]["probe_id"], "runtime");
    assert!(
        results["members"]
            .as_array()
            .is_some_and(|members| members.iter().all(|member| member["status"] == "completed"))
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 3);
    assert!(projection.threads.values().all(|thread| {
        thread.status == sigil_kernel::AgentThreadStatus::Completed
            && thread.result.as_ref().is_some_and(|result| {
                result.status == sigil_kernel::AgentThreadTerminalStatus::Completed
            })
    }));
    let batch = projection
        .batches
        .values()
        .next()
        .expect("planner discovery batch projection");
    assert_eq!(results["batch_id"].as_str(), Some(batch.batch_id.as_str()));
    let parent_thread_id = batch
        .parent_thread_id
        .as_ref()
        .expect("planner discovery batch should retain its planner parent");
    assert!(
        projection
            .threads
            .get(parent_thread_id)
            .is_some_and(|thread| thread.batch_id.is_none())
    );
    assert_eq!(batch.member_thread_ids.len(), 2);
    assert_eq!(
        batch
            .member_keys
            .keys()
            .map(AgentRouteId::as_str)
            .collect::<Vec<_>>(),
        vec!["kernel", "runtime"]
    );
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn planner_discovery_rejects_overlapping_batch_without_consuming_valid_retry() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 4;
    let supervisor = supervisor_with_budget(budget)?;
    let observed_error = Arc::new(Mutex::new(None));
    let starts = Arc::new(AtomicUsize::new(0));
    let mut explore_tools = ToolRegistry::new();
    explore_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(RejectedDiscoveryPlannerProvider {
                observed_error: Arc::clone(&observed_error),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            explore_tools,
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    )
    .with_planner_discovery_policy(MultiAgentMode::ExplicitRequestOnly, 3);
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_rejected_planner_discovery")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let cancellation = RunCancellationOwner::new();
    let planner_input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("plan the task")])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: task_id.clone(),
                max_plan_steps: 12,
                max_plan_versions: 3,
                worktree_availability:
                    TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
            })
            .with_cancellation(cancellation.handle());
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "inspect runtime before implementation".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: planner_input,
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.accepted_plan.plan_version, 1);
    assert_eq!(
        starts.load(Ordering::SeqCst),
        1,
        "the rejected batch must start no provider, while one corrected batch remains admissible"
    );
    assert!(
        observed_error
            .lock()
            .expect("planner discovery error observation lock should not be poisoned")
            .as_deref()
            .is_some_and(|error| error.contains("whole_batch_rejected"))
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 2);
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn planner_discovery_allows_only_one_batch_per_planning_attempt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 4;
    let supervisor = supervisor_with_budget(budget)?;
    let observed_rejection = Arc::new(Mutex::new(None));
    let starts = Arc::new(AtomicUsize::new(0));
    let mut explore_tools = ToolRegistry::new();
    explore_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new_with_task_roles(
        supervisor.clone(),
        Agent::new(
            Box::new(RepeatedDiscoveryPlannerProvider {
                observed_rejection: Arc::clone(&observed_rejection),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "executor done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            explore_tools,
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "synthesis done",
            }),
            ToolRegistry::new(),
        ),
    )
    .with_planner_discovery_policy(MultiAgentMode::ExplicitRequestOnly, 3);
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let task_id = TaskId::new("task_repeated_planner_discovery")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let child_session_ref = task_participant_session_ref(&task_id, &attempt_id)?;
    let cancellation = RunCancellationOwner::new();
    let planner_input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user("plan the task")])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: task_id.clone(),
                max_plan_steps: 12,
                max_plan_versions: 3,
                worktree_availability:
                    TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
            })
            .with_cancellation(cancellation.handle());
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "inspect runtime before implementation".to_owned(),
                },
                attempt_id,
                child_session_ref,
                child_input: planner_input,
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.accepted_plan.plan_version, 1);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(
        observed_rejection
            .lock()
            .expect("planner discovery rejection lock should not be poisoned")
            .as_deref()
            .is_some_and(|rejection| rejection.contains("whole_batch_rejected"))
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 2);
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn supervisor_records_post_run_usage_without_budget_warning() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(UsageProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage")?,
                attempt_id: participant_attempt_id_for("usage")?,
                child_session_ref: participant_session_ref_for("usage")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "too expensive");
    let projection = session.agent_thread_state_projection();
    let thread = projection.latest_thread().expect("child thread");
    assert_eq!(thread.status, sigil_kernel::AgentThreadStatus::Completed);
    assert_eq!(
        thread
            .result
            .as_ref()
            .and_then(|result| result.usage.as_ref())
            .map(|usage| usage.total_tokens),
        Some(13)
    );
    assert!(!handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(message) if message.contains("agent budget warning"))
    }));
    Ok(())
}

#[tokio::test]
async fn task_read_batch_overlaps_provider_runs_and_commits_in_request_order() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let completion_order = Arc::new(Mutex::new(Vec::new()));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(ParallelTaskChildProvider {
                barrier,
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
                completion_order: Arc::clone(&completion_order),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect in parallel".to_owned(),
    };
    let requests = ["read_a", "read_b"]
        .into_iter()
        .map(|step_id| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: step(step_id)?,
                attempt_id: participant_attempt_id_for(step_id)?,
                child_session_ref: participant_session_ref_for(step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let expected_attempts = requests
        .iter()
        .map(|request| request.attempt_id.clone())
        .collect::<Vec<_>>();
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let preparation =
        runner.prepare_child_session_batch(&mut session, requests, &mut handler, &mut approval)?;
    session.append_control(ControlEntry::Note {
        kind: "parent_borrow_boundary_probe".to_owned(),
        data: serde_json::json!({"available_during_child_await": true}),
    })?;
    let commit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        expect_detached_task_batch(preparation),
    )
    .await
    .expect("parallel provider barrier should complete")?;
    assert_eq!(commit.request_count(), 2);
    let outputs = commit.commit(&mut session, &mut handler)?;

    assert_eq!(outputs.len(), 2);
    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(
        *completion_order
            .lock()
            .expect("completion order should not be poisoned"),
        vec!["read_b", "read_a"]
    );
    assert_eq!(
        outputs
            .into_iter()
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .map(|output| output.attempt_id)
            .collect::<Vec<_>>(),
        expected_attempts
    );
    let projection = session.agent_thread_state_projection();
    assert_eq!(projection.threads.len(), 2);
    assert!(
        projection
            .threads
            .values()
            .all(|thread| thread.status == sigil_kernel::AgentThreadStatus::Completed)
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskChildSession(child))
                    if child.status == TaskChildSessionStatus::Completed =>
                {
                    Some(child.step_id.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec!["read_a", "read_b"],
        "parent terminal commits should remain in stable request order"
    );
    let progress = supervisor.task_completion_progress();
    let batch = progress.batch.expect("task completion progress");
    assert_eq!(batch.task_id, "task_1");
    assert_eq!(batch.plan_version, 1);
    assert_eq!(batch.arrived, 2);
    assert_eq!(batch.total, 2);
    assert_eq!(batch.members[0].step_id, "read_a");
    assert_eq!(batch.members[0].request_order, 1);
    assert_eq!(batch.members[0].arrival_order, Some(2));
    assert_eq!(batch.members[1].step_id, "read_b");
    assert_eq!(batch.members[1].request_order, 2);
    assert_eq!(batch.members[1].arrival_order, Some(1));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn task_parallel_approval_aggregates_only_exact_matching_routes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(ToolCallingChildProvider), tools),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_approval_batch")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect identical inputs in parallel".to_owned(),
    };
    let requests = ["read_a", "read_b"]
        .into_iter()
        .map(|step_id| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: step(step_id)?,
                attempt_id: task_participant_attempt_id(
                    &task.task_id,
                    TaskParticipantPurpose::Step,
                    Some(1),
                    Some(&TaskStepId::new(step_id)?),
                    1,
                )?,
                child_session_ref: task_participant_session_ref(
                    &task.task_id,
                    &task_participant_attempt_id(
                        &task.task_id,
                        TaskParticipantPurpose::Step,
                        Some(1),
                        Some(&TaskStepId::new(step_id)?),
                        1,
                    )?,
                )?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = CountingApprovalHandler::default();

    let preparation =
        runner.prepare_child_session_batch(&mut session, requests, &mut handler, &mut approval)?;
    let commit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        expect_detached_task_batch(preparation),
    )
    .await
    .expect("aggregated approvals should not leave a follower waiting")?;
    commit
        .commit(&mut session, &mut handler)?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(approval.decisions, 1);
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolApprovalRequested { .. }))
            .count(),
        1
    );
    let routes = task_approval_routes(&session);
    assert_eq!(routes.len(), 4);
    assert_eq!(
        routes
            .iter()
            .filter(|route| route.status == TaskRouteStatus::Requested)
            .count(),
        2
    );
    assert_eq!(
        routes
            .iter()
            .filter(|route| route.status == TaskRouteStatus::Resolved)
            .count(),
        2
    );
    let bindings = routes
        .iter()
        .map(|route| route.binding.as_ref().expect("exact route binding"))
        .collect::<Vec<_>>();
    assert!(bindings.iter().all(|binding| {
        binding.batch_id == bindings[0].batch_id
            && binding.batch_id.starts_with("task_approval_batch_v1_")
            && binding.batch_id.len() == "task_approval_batch_v1_".len() + 64
            && binding.permission_signature == bindings[0].permission_signature
            && binding.policy_fingerprint == bindings[0].policy_fingerprint
            && binding.aggregation_signature == bindings[0].aggregation_signature
    }));
    assert_eq!(
        bindings
            .iter()
            .map(|binding| binding.source_thread_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    assert_eq!(
        bindings
            .iter()
            .map(|binding| binding.attempt_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    let agent_bindings = agent_approval_routes(&session)
        .into_iter()
        .map(|route| route.binding.as_ref().expect("exact agent route binding"))
        .collect::<Vec<_>>();
    assert_eq!(agent_bindings.len(), 4);
    assert!(agent_bindings.iter().all(|binding| {
        binding
            .batch_id
            .as_ref()
            .is_some_and(|batch_id| batch_id.as_str() == bindings[0].batch_id)
            && binding.permission_signature == bindings[0].permission_signature
            && binding.policy_fingerprint == bindings[0].policy_fingerprint
            && binding.source_workspace_id == bindings[0].source_workspace_id
            && binding.isolation == bindings[0].isolation
    }));
    assert_eq!(
        agent_bindings
            .iter()
            .map(|binding| binding.attempt_id.as_str())
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn task_parallel_approval_does_not_aggregate_distinct_tool_arguments() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(DistinctToolCallingChildProvider), tools),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_distinct_approval_batch")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect distinct inputs in parallel".to_owned(),
    };
    let requests = ["read_a", "read_b"]
        .into_iter()
        .map(|step_id| {
            let step_id = TaskStepId::new(step_id)?;
            let attempt_id = task_participant_attempt_id(
                &task.task_id,
                TaskParticipantPurpose::Step,
                Some(1),
                Some(&step_id),
                1,
            )?;
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: step(step_id.as_str())?,
                child_session_ref: task_participant_session_ref(&task.task_id, &attempt_id)?,
                attempt_id,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {}", step_id.as_str())),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = CountingApprovalHandler::default();

    let preparation =
        runner.prepare_child_session_batch(&mut session, requests, &mut handler, &mut approval)?;
    let commit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        expect_detached_task_batch(preparation),
    )
    .await
    .expect("distinct approvals should both resolve")?;
    commit
        .commit(&mut session, &mut handler)?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(approval.decisions, 2);
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::ToolApprovalRequested { .. }))
            .count(),
        2
    );
    assert_eq!(
        task_approval_routes(&session)
            .iter()
            .filter_map(|route| {
                route
                    .binding
                    .as_ref()
                    .map(|binding| binding.aggregation_signature.as_str())
            })
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn task_parallel_approval_releases_followers_when_presentation_fails() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(ToolCallingChildProvider), tools),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_failed_approval_presenter")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "fail the shared presenter".to_owned(),
    };
    let requests = ["read_a", "read_b"]
        .into_iter()
        .map(|step_id| {
            let step_id = TaskStepId::new(step_id)?;
            let attempt_id = task_participant_attempt_id(
                &task.task_id,
                TaskParticipantPurpose::Step,
                Some(1),
                Some(&step_id),
                1,
            )?;
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: step(step_id.as_str())?,
                child_session_ref: task_participant_session_ref(&task.task_id, &attempt_id)?,
                attempt_id,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {}", step_id.as_str())),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RejectApprovalPresentationHandler::default();
    let mut approval = CountingApprovalHandler::default();

    let preparation =
        runner.prepare_child_session_batch(&mut session, requests, &mut handler, &mut approval)?;
    let commit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        expect_detached_task_batch(preparation),
    )
    .await
    .expect("a failed leader presenter must wake every aggregated follower")?;
    let outputs = commit.commit(&mut session, &mut handler)?;

    assert_eq!(handler.requests, 1);
    assert_eq!(approval.decisions, 0);
    assert_eq!(outputs.len(), 2);
    assert!(outputs.into_iter().all(|output| output.is_err()));
    Ok(())
}

#[tokio::test]
async fn task_changeset_batch_overlaps_providers_and_returns_snapshot_bound_proposals() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let logs = tempfile::tempdir()?;
    std::fs::write(temp.path().join("base.txt"), "unchanged\n")?;
    let workspace_id = sigil_kernel::stable_workspace_id(temp.path())?;
    let base_snapshot_id = sigil_kernel::build_workspace_snapshot(
        temp.path(),
        workspace_id,
        &sigil_kernel::VerificationScope::all_tracked(
            sigil_kernel::DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
        ),
        0,
    )?
    .workspace_snapshot_id
    .ok_or_else(|| anyhow!("test workspace snapshot should be complete"))?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let completion_order = Arc::new(Mutex::new(Vec::new()));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(TextProvider { text: "read done" }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(ParallelChangesetChildProvider {
                barrier,
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
                completion_order: Arc::clone(&completion_order),
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "propose changes in parallel".to_owned(),
    };
    let requests = ["proposal_a", "proposal_b"]
        .into_iter()
        .map(|step_id| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: changeset_step(step_id)?,
                attempt_id: participant_attempt_id_for(step_id)?,
                child_session_ref: participant_session_ref_for(step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("propose {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: Some(base_snapshot_id.clone()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let expected_attempts = requests
        .iter()
        .map(|request| request.attempt_id.clone())
        .collect::<Vec<_>>();
    let parent_store = JsonlSessionStore::new(logs.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let preparation =
        runner.prepare_child_session_batch(&mut session, requests, &mut handler, &mut approval)?;
    let commit = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        expect_detached_task_batch(preparation),
    )
    .await
    .expect("parallel changeset provider barrier should complete")?;
    let outputs = commit
        .commit(&mut session, &mut handler)?
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(
        *completion_order
            .lock()
            .expect("completion order should not be poisoned"),
        vec!["proposal_b", "proposal_a"]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.attempt_id.clone())
            .collect::<Vec<_>>(),
        expected_attempts
    );
    assert!(outputs.iter().all(|output| {
        output.changeset_proposal.is_some()
            && output.isolated_parent_snapshot_id.as_deref() == Some(base_snapshot_id.as_str())
    }));
    let artifact_recorder = session
        .mutation_event_recorder()
        .expect("parallel changeset task should own a mutation artifact recorder");
    for output in &outputs {
        let proposal = output
            .changeset_proposal
            .as_ref()
            .expect("parallel changeset child should return a proposal");
        assert!(
            proposal
                .artifact_ref
                .starts_with("mutation-artifact:sha256:")
        );
        assert_eq!(
            artifact_recorder.read_immutable_content_artifact(&proposal.artifact_ref)?,
            proposal.artifact.content.as_bytes()
        );
        assert_eq!(
            proposal.integration_facts.changeset_artifact_ref,
            proposal.artifact_ref
        );
    }
    assert_eq!(
        std::fs::read_to_string(temp.path().join("base.txt"))?,
        "unchanged\n"
    );
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn task_changeset_batch_rejects_missing_base_snapshot_before_provider_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(TextProvider { text: "read done" }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "propose changes in parallel".to_owned(),
    };
    let requests = ["proposal_a", "proposal_b"]
        .into_iter()
        .map(|step_id| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: changeset_step(step_id)?,
                attempt_id: participant_attempt_id_for(step_id)?,
                child_session_ref: participant_session_ref_for(step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("propose {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let outputs = runner
        .run_child_session_batch(&mut session, requests, &mut handler, &mut approval)
        .await?;

    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().all(|output| {
        output
            .as_ref()
            .err()
            .is_some_and(|error| format!("{error:#}").contains("missing its parent base snapshot"))
    }));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(session.agent_thread_state_projection().threads.is_empty());
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn task_changeset_batch_rejects_mixed_base_snapshots_before_provider_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(TextProvider { text: "read done" }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "propose changes in parallel".to_owned(),
    };
    let requests = ["proposal_a", "proposal_b"]
        .into_iter()
        .enumerate()
        .map(|(index, step_id)| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: changeset_step(step_id)?,
                attempt_id: participant_attempt_id_for(step_id)?,
                child_session_ref: participant_session_ref_for(step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("propose {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: Some(format!("snapshot-{index}")),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let outputs = runner
        .run_child_session_batch(&mut session, requests, &mut handler, &mut approval)
        .await?;

    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().all(|output| {
        output
            .as_ref()
            .err()
            .is_some_and(|error| format!("{error:#}").contains("mixes parent base snapshots"))
    }));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(session.agent_thread_state_projection().threads.is_empty());
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

async fn expect_detached_task_batch(
    preparation: TaskChildSessionBatchPreparation<'_>,
) -> Result<TaskChildSessionBatchCommitEnvelope> {
    match preparation {
        TaskChildSessionBatchPreparation::Detached(batch_future) => batch_future.await,
        TaskChildSessionBatchPreparation::Fallback(_) => {
            panic!("runtime task batch should use detached execution");
        }
    }
}

#[tokio::test]
async fn task_read_batch_rejects_capacity_before_any_provider_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut budget = AgentBudgetPolicy::from_root_config(&root_config());
    budget.max_subagents = 1;
    let supervisor = supervisor_with_budget(budget)?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect in parallel".to_owned(),
    };
    let requests = ["read_a", "read_b"]
        .into_iter()
        .map(|step_id| {
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step: step(step_id)?,
                attempt_id: participant_attempt_id_for(step_id)?,
                child_session_ref: participant_session_ref_for(step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let outputs = runner
        .run_child_session_batch(&mut session, requests, &mut handler, &mut approval)
        .await?;

    assert_eq!(outputs.len(), 2);
    assert!(outputs.iter().all(|output| output.is_err()));
    assert!(outputs.iter().all(|output| {
        output.as_ref().err().is_some_and(|error| {
            let message = format!("{error:#}");
            message.contains("rejected before provider dispatch")
                && message.contains("active=0 requested=2")
                && message.contains("[task].max_subagents=1")
        })
    }));
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(session.agent_thread_state_projection().threads.is_empty());
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskChildSession(child))
                if child.status == TaskChildSessionStatus::Started
        )
    }));
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn task_read_batch_rejects_member_preflight_before_any_provider_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(CountingDiscoveryProvider {
                starts: Arc::clone(&starts),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect in parallel".to_owned(),
    };
    let requests = [step("read_a")?, write_step("write_b")?]
        .into_iter()
        .map(|step| {
            let step_id = step.step_id.as_str().to_owned();
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step,
                attempt_id: participant_attempt_id_for(&step_id)?,
                child_session_ref: participant_session_ref_for(&step_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("inspect {step_id}")),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let outputs = runner
        .run_child_session_batch(&mut session, requests, &mut handler, &mut approval)
        .await?;

    assert_eq!(outputs.len(), 2);
    let errors = outputs
        .iter()
        .map(|output| {
            output
                .as_ref()
                .err()
                .map(|error| format!("{error:#}"))
                .unwrap_or_else(|| "unexpected success".to_owned())
        })
        .collect::<Vec<_>>();
    assert!(
        errors.iter().all(|error| error
            .contains("parallel task child writer requires changeset-only or worktree isolation")),
        "unexpected batch errors: {errors:#?}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(session.agent_thread_state_projection().threads.is_empty());
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn planner_rate_limit_preserves_zero_consumption_retry_proof() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = task_role_runner_with_rate_limited_participant(
        supervisor,
        TaskParticipantPurpose::Planner,
        Arc::clone(&starts),
    );
    let task_id = TaskId::new("task_planner_rate_limit")?;
    let attempt_id =
        task_participant_attempt_id(&task_id, TaskParticipantPurpose::Planner, None, None, 1)?;
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_planner_session(
            &mut session,
            TaskPlannerSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "plan after provider pressure".to_owned(),
                },
                attempt_id: attempt_id.clone(),
                child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("plan the task"),
                ])
                .with_logical_run_id(task_participant_logical_run_id(&attempt_id)),
                options: run_options(temp.path().to_path_buf()),
                discovery_options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await
        .expect_err("planner 429 should remain retryable");

    assert!(
        error
            .downcast_ref::<TaskParticipantRetryError>()
            .is_some_and(|retry| matches!(
                retry.proof(),
                TaskParticipantRetryProof::ProviderConfirmedNoConsumption {
                    zero_output: true,
                    zero_tool: true,
                    zero_effect: true,
                    ..
                }
            ))
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn synthesis_rate_limit_preserves_zero_consumption_retry_proof() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = task_role_runner_with_rate_limited_participant(
        supervisor,
        TaskParticipantPurpose::Synthesis,
        Arc::clone(&starts),
    );
    let task_id = TaskId::new("task_synthesis_rate_limit")?;
    let attempt_id = task_participant_attempt_id(
        &task_id,
        TaskParticipantPurpose::Synthesis,
        Some(1),
        None,
        1,
    )?;
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::new("deepseek", "deepseek-v4-flash").with_store(parent_store);
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_synthesis_session(
            &mut session,
            TaskSynthesisSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: task_id.clone(),
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "synthesize after provider pressure".to_owned(),
                },
                attempt_id: attempt_id.clone(),
                child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
                plan_version: 1,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("synthesize the task"),
                ])
                .with_logical_run_id(task_participant_logical_run_id(&attempt_id)),
                options: run_options(temp.path().to_path_buf()),
            },
            &mut handler,
            &mut approval,
        )
        .await
        .expect_err("synthesis 429 should remain retryable");

    assert!(
        error
            .downcast_ref::<TaskParticipantRetryError>()
            .is_some_and(|retry| matches!(
                retry.proof(),
                TaskParticipantRetryProof::ProviderConfirmedNoConsumption {
                    zero_output: true,
                    zero_tool: true,
                    zero_effect: true,
                    ..
                }
            ))
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn task_rate_limit_retry_proof_survives_a_confirmed_connect_retry_prefix() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(ConnectThenRateLimitedTaskChildProvider {
                starts: Arc::clone(&starts),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let attempt_id = participant_attempt_id_for("read_connect_then_rate_limit")?;
    let child_session_ref = participant_session_ref_for("read_connect_then_rate_limit")?;
    let request = TaskChildSessionRunRequest {
        task: sigil_kernel::SequentialTaskRequest {
            task_id: TaskId::new("task_connect_then_rate_limit")?,
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "retain a safe rate-limit retry proof after connect retry".to_owned(),
        },
        plan_version: 1,
        step: step("read_connect_then_rate_limit")?,
        attempt_id: attempt_id.clone(),
        child_session_ref: child_session_ref.clone(),
        child_input: AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
            "inspect after a transient connect failure",
        )])
        .with_logical_run_id(task_participant_logical_run_id(&attempt_id)),
        options: run_options(temp.path().to_path_buf()),
        isolated_base_snapshot_id: None,
    };
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_child_session(&mut session, request, &mut handler, &mut approval)
        .await
        .expect_err("the final rate-limit attempt should remain retryable");

    assert_eq!(starts.load(Ordering::SeqCst), 2);
    assert!(
        error
            .downcast_ref::<TaskParticipantRetryError>()
            .is_some_and(|retry| matches!(
                retry.proof(),
                TaskParticipantRetryProof::ProviderConfirmedNoConsumption {
                    zero_output: true,
                    zero_tool: true,
                    zero_effect: true,
                    ..
                }
            ))
    );
    let child_store = JsonlSessionStore::new(child_session_ref.resolve(temp.path()))?;
    let child = Session::load_from_store("deepseek", "deepseek-v4-flash", child_store)?;
    let projection = child.provider_physical_attempt_projection()?;
    let attempts =
        projection.attempts_for_logical_run_id(&task_participant_logical_run_id(&attempt_id));
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts[0]
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.rejection),
        Some(ProviderRequestRejection::ConnectFailedBeforeDispatch)
    );
    assert_eq!(
        attempts[1]
            .terminal
            .as_ref()
            .and_then(|terminal| terminal.rejection),
        Some(ProviderRequestRejection::RateLimited)
    );
    Ok(())
}

#[tokio::test]
async fn task_provider_rate_limit_blocks_rebuilt_runner_before_provider_dispatch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let starts = Arc::new(AtomicUsize::new(0));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(RateLimitedTaskChildProvider {
                starts: Arc::clone(&starts),
                rate_limits_remaining: Arc::new(AtomicUsize::new(1)),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: TaskId::new("task_1")?,
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "inspect with provider backpressure".to_owned(),
    };
    let request_for = |step_id: &str| -> Result<TaskChildSessionRunRequest> {
        let attempt_id = participant_attempt_id_for(step_id)?;
        Ok(TaskChildSessionRunRequest {
            task: task.clone(),
            plan_version: 1,
            step: step(step_id)?,
            attempt_id: attempt_id.clone(),
            child_session_ref: participant_session_ref_for(step_id)?,
            child_input: AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                format!("inspect {step_id}"),
            )])
            .with_logical_run_id(task_participant_logical_run_id(&attempt_id)),
            options: run_options(temp.path().to_path_buf()),
            isolated_base_snapshot_id: None,
        })
    };
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let first = runner
        .run_child_session_batch(
            &mut session,
            vec![request_for("read_a")?],
            &mut handler,
            &mut approval,
        )
        .await?;
    assert_eq!(first.len(), 1);
    assert!(
        first[0].as_ref().err().is_some_and(|error| {
            error
                .downcast_ref::<TaskParticipantRetryError>()
                .is_some_and(|retry| {
                    retry.retry_after_ms() > 0
                        && matches!(
                            retry.proof(),
                            TaskParticipantRetryProof::ProviderConfirmedNoConsumption {
                                zero_output: true,
                                zero_tool: true,
                                zero_effect: true,
                                ..
                            }
                        )
                })
                && format!("{error:#}").contains("test provider rate limited")
        }),
        "first error: {:#}",
        first[0].as_ref().expect_err("first request should fail")
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);

    let resumed_runner = AgentSupervisorTaskChildRunner::new(
        supervisor.clone(),
        Agent::new(
            Box::new(RateLimitedTaskChildProvider {
                starts: Arc::clone(&starts),
                rate_limits_remaining: Arc::new(AtomicUsize::new(0)),
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let blocked = resumed_runner
        .run_child_session_batch(
            &mut session,
            vec![request_for("read_b")?, request_for("read_c")?],
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(blocked.len(), 2);
    assert!(
        blocked.iter().all(|output| {
            output.as_ref().err().is_some_and(|error| {
                let message = format!("{error:#}");
                error
                    .downcast_ref::<TaskParticipantRetryError>()
                    .is_some_and(|retry| {
                        retry.retry_after_ms() > 0
                            && matches!(
                                retry.proof(),
                                TaskParticipantRetryProof::AdmissionRejectedBeforeDispatch {
                                    zero_output: true,
                                    zero_tool: true,
                                    zero_effect: true,
                                }
                            )
                    })
                    && message.contains("provider route is cooling down")
                    && message.contains("rejected before provider dispatch")
            })
        }),
        "blocked errors: {blocked:#?}"
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(session.agent_thread_state_projection().threads.len(), 1);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(sigil_kernel::ControlEntry::TaskChildSession(child))
                        if child.status == TaskChildSessionStatus::Started
                )
            })
            .count(),
        1
    );
    assert!(supervisor.active_profile_ids().is_empty());
    Ok(())
}

#[tokio::test]
async fn task_provider_rate_limit_after_output_is_not_retryable() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(OutputThenRateLimitedTaskChildProvider),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let attempt_id = participant_attempt_id_for("read_after_output")?;
    let child_session_ref = participant_session_ref_for("read_after_output")?;
    let request = TaskChildSessionRunRequest {
        task: sigil_kernel::SequentialTaskRequest {
            task_id: TaskId::new("task_after_output")?,
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "do not duplicate provider output".to_owned(),
        },
        plan_version: 1,
        step: step("read_after_output")?,
        attempt_id: attempt_id.clone(),
        child_session_ref: child_session_ref.clone(),
        child_input: AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
            "inspect after output",
        )])
        .with_logical_run_id(task_participant_logical_run_id(&attempt_id)),
        options: run_options(temp.path().to_path_buf()),
        isolated_base_snapshot_id: None,
    };
    let parent_store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", parent_store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_child_session(&mut session, request, &mut handler, &mut approval)
        .await
        .expect_err("provider fails after emitting output");

    assert!(error.downcast_ref::<TaskParticipantRetryError>().is_none());
    assert!(format!("{error:#}").contains("rate limited after output"));
    let child_store = JsonlSessionStore::new(child_session_ref.resolve(temp.path()))?;
    let child = Session::load_from_store("deepseek", "deepseek-v4-flash", child_store)?;
    let projection = child.provider_physical_attempt_projection()?;
    let attempts =
        projection.attempts_for_logical_run_id(&task_participant_logical_run_id(&attempt_id));
    assert_eq!(attempts.len(), 1);
    assert_eq!(
        attempts[0]
            .terminal
            .as_ref()
            .map(|terminal| terminal.outcome),
        Some(ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput)
    );
    Ok(())
}

#[tokio::test]
async fn supervisor_records_cumulative_agent_tokens_without_denial() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let supervisor = supervisor_with_budget(AgentBudgetPolicy::from_root_config(&root_config()))?;
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(UsageProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage_one")?,
                attempt_id: participant_attempt_id_for("usage_one")?,
                child_session_ref: participant_session_ref_for("usage_one")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage_two")?,
                attempt_id: participant_attempt_id_for("usage_two")?,
                child_session_ref: participant_session_ref_for("usage_two")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill again"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("usage_three")?,
                attempt_id: participant_attempt_id_for("usage_three")?,
                child_session_ref: participant_session_ref_for("usage_three")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill after budget"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    let projection = session.agent_thread_state_projection();
    let completed_usage = projection
        .threads
        .values()
        .filter(|thread| thread.status == sigil_kernel::AgentThreadStatus::Completed)
        .filter_map(|thread| {
            thread
                .result
                .as_ref()
                .and_then(|result| result.usage.as_ref())
                .map(|usage| usage.total_tokens)
        })
        .collect::<Vec<_>>();
    assert_eq!(completed_usage, vec![13, 13, 13]);
    assert!(!projection.threads.values().any(|thread| {
        thread
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("token budget"))
    }));
    Ok(())
}

#[tokio::test]
async fn child_run_context_uses_selected_role_provider_capabilities() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut config = root_config();
    config.task.allow_write_subagents = false;
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider { text: "read done" }),
            ToolRegistry::new(),
        ),
        Agent::new(Box::new(ResultReplayProvider), ToolRegistry::new()),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke writer".to_owned(),
                },
                plan_version: 1,
                step: write_step("inspect")?,
                attempt_id: participant_attempt_id_for("inspect")?,
                child_session_ref: participant_session_ref_for("inspect")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("inspect only"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    let mut expected = provider_capabilities();
    expected.supports_agent_result_replay = true;
    let expected_hash = provider_capability_hash(&expected)?;
    let projection = session.agent_thread_state_projection();
    let thread = projection
        .threads
        .values()
        .next()
        .expect("agent thread projected");
    assert_eq!(
        thread
            .run_context
            .as_ref()
            .map(|context| context.provider_capability_hash.as_str()),
        Some(expected_hash.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn direct_child_skill_uses_supervisor() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider { text: "child done" }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("invoke_skill")?,
                attempt_id: participant_attempt_id_for("invoke_skill")?,
                child_session_ref: participant_session_ref_for("invoke_skill")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "child done");
    let agent_projection = session.agent_thread_state_projection();
    assert_eq!(agent_projection.threads.len(), 1);
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&TaskId::new("task_1")?)
        .expect("task child session projected");
    assert_eq!(task.child_sessions.len(), 1);
    assert!(!handler.events.iter().any(|event| matches!(
        event,
        RunEvent::AssistantMessage(_) | RunEvent::TextDelta(_)
    )));
    Ok(())
}

#[tokio::test]
async fn child_tool_approval_routes_are_audited_and_stored() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut read_tools = ToolRegistry::new();
    read_tools.register(Arc::new(ApprovalRouteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(ToolCallingChildProvider), read_tools),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let store = JsonlSessionStore::new(temp.path().join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("approval_route")?,
                attempt_id: participant_attempt_id_for("approval_route")?,
                child_session_ref: participant_session_ref_for("approval_route")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("read through approval"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(output.final_text, "tool route done");
    let agent_statuses = agent_route_statuses(&session);
    assert!(agent_statuses.contains(&sigil_kernel::AgentRouteStatus::Requested));
    assert!(agent_statuses.contains(&sigil_kernel::AgentRouteStatus::Resolved));
    let task_statuses = task_route_statuses(&session);
    assert!(task_statuses.contains(&TaskRouteStatus::Requested));
    assert!(task_statuses.contains(&TaskRouteStatus::Resolved));
    let task_routes = task_approval_routes(&session);
    assert_eq!(task_routes.len(), 2);
    let requested_binding = task_routes[0]
        .binding
        .as_ref()
        .expect("requested approval route has an exact binding");
    let resolved_binding = task_routes[1]
        .binding
        .as_ref()
        .expect("resolved approval route has an exact binding");
    assert_eq!(requested_binding, resolved_binding);
    assert!(
        requested_binding
            .batch_id
            .starts_with("task_approval_batch_v1_")
    );
    assert_eq!(
        requested_binding.batch_id.len(),
        "task_approval_batch_v1_".len() + 64
    );
    assert_eq!(
        requested_binding.attempt_id,
        participant_attempt_id_for("approval_route")?
    );
    assert!(
        requested_binding
            .source_thread_id
            .as_str()
            .starts_with("agent_v1_")
    );
    assert!(
        requested_binding
            .permission_signature
            .starts_with("sha256:")
    );
    assert!(requested_binding.policy_fingerprint.starts_with("sha256:"));
    assert_eq!(
        requested_binding.source_workspace_id,
        stable_workspace_id(temp.path())?
    );
    assert_eq!(
        requested_binding.isolation,
        TaskIsolationMode::SharedReadOnly
    );
    let agent_routes = agent_approval_routes(&session);
    assert_eq!(agent_routes.len(), 2);
    let agent_requested_binding = agent_routes[0]
        .binding
        .as_ref()
        .expect("requested agent route has an exact binding");
    let agent_resolved_binding = agent_routes[1]
        .binding
        .as_ref()
        .expect("resolved agent route has an exact binding");
    assert_eq!(agent_requested_binding, agent_resolved_binding);
    assert_eq!(
        agent_requested_binding
            .batch_id
            .as_ref()
            .expect("task agent route batch")
            .as_str(),
        requested_binding.batch_id
    );
    assert_eq!(
        agent_requested_binding.permission_signature,
        requested_binding.permission_signature
    );
    assert_eq!(
        agent_requested_binding.source_workspace_id,
        requested_binding.source_workspace_id
    );
    assert!(
        participant_session_ref_for("approval_route")?
            .resolve(temp.path())
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn failed_child_does_not_append_successful_parent_answer() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(Box::new(FailingProvider), ToolRegistry::new()),
        Agent::new(
            Box::new(TextProvider {
                text: "writer done",
            }),
            ToolRegistry::new(),
        ),
    );
    let mut session = Session::new("deepseek", "deepseek-v4-flash");
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let result = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_1")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "invoke agent".to_owned(),
                },
                plan_version: 1,
                step: step("invoke_skill")?,
                attempt_id: participant_attempt_id_for("invoke_skill")?,
                child_session_ref: participant_session_ref_for("invoke_skill")?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("apply skill"),
                ]),
                options: run_options(temp.path().to_path_buf()),
                isolated_base_snapshot_id: None,
            },
            &mut handler,
            &mut approval,
        )
        .await;

    assert!(result.is_err());
    assert!(session.messages().is_empty());
    let task_projection = session.task_state_projection();
    let task = task_projection
        .tasks
        .get(&TaskId::new("task_1")?)
        .expect("task child session projected");
    assert!(
        task.child_sessions
            .values()
            .any(|child| child.status == sigil_kernel::TaskChildSessionStatus::Failed)
    );
    Ok(())
}

#[tokio::test]
async fn task_worktree_batch_overlaps_providers_and_preserves_parent_workspace() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let repository_root = temp.path().join("repository");
    initialize_worktree_test_repository(&repository_root)?;
    fs::write(repository_root.join("base.txt"), "user dirty baseline\n")?;
    fs::write(
        repository_root.join("user-notes.txt"),
        "safe untracked baseline\n",
    )?;
    let base_snapshot_id = worktree_test_snapshot_id(&repository_root)?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["write_file".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let completion_order = Arc::new(Mutex::new(Vec::new()));
    let mut write_tools = ToolRegistry::new();
    write_tools.register(Arc::new(WorktreeWriteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider {
                text: "read complete",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(ParallelWorktreeWriteProvider {
                barrier,
                active: Arc::clone(&active),
                max_active: Arc::clone(&max_active),
                completion_order: Arc::clone(&completion_order),
            }),
            write_tools,
        ),
    );
    let logs_root = temp.path().join("logs");
    fs::create_dir(&logs_root)?;
    let store = JsonlSessionStore::new(logs_root.join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let task_id = TaskId::new("task_parallel_worktree")?;
    let task = sigil_kernel::SequentialTaskRequest {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
        objective: "edit independent files in parallel worktrees".to_owned(),
    };
    let requests = ["worktree_a", "worktree_b"]
        .into_iter()
        .map(|step_id| {
            let step = worktree_step(step_id)?;
            let attempt_id = task_participant_attempt_id(
                &task_id,
                TaskParticipantPurpose::Step,
                Some(1),
                Some(&step.step_id),
                1,
            )?;
            Ok(TaskChildSessionRunRequest {
                task: task.clone(),
                plan_version: 1,
                step,
                child_session_ref: task_participant_session_ref(&task_id, &attempt_id)?,
                attempt_id,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user(format!("edit the isolated file for {step_id}")),
                ]),
                options: run_options(repository_root.clone()),
                isolated_base_snapshot_id: Some(base_snapshot_id.clone()),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let expected_attempts = requests
        .iter()
        .map(|request| request.attempt_id.clone())
        .collect::<Vec<_>>();
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let outputs = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runner.run_child_session_batch(&mut session, requests, &mut handler, &mut approval),
    )
    .await
    .expect("parallel worktree provider barrier should complete")?
    .into_iter()
    .collect::<Result<Vec<_>>>()?;

    assert_eq!(max_active.load(Ordering::SeqCst), 2);
    assert_eq!(
        *completion_order
            .lock()
            .expect("completion order should not be poisoned"),
        vec!["worktree_b", "worktree_a"]
    );
    assert_eq!(
        outputs
            .iter()
            .map(|output| output.attempt_id.clone())
            .collect::<Vec<_>>(),
        expected_attempts
    );
    assert!(outputs.iter().all(|output| {
        output.outcome.changed_files.is_empty()
            && output.isolated_parent_snapshot_id.as_deref() == Some(base_snapshot_id.as_str())
            && output.changeset_proposal.is_some()
    }));
    let artifact_recorder = session
        .mutation_event_recorder()
        .expect("parallel durable task should own a mutation artifact recorder");
    for output in &outputs {
        let proposal = output
            .changeset_proposal
            .as_ref()
            .expect("parallel worktree child should return a proposal");
        assert!(
            proposal
                .artifact_ref
                .starts_with("mutation-artifact:sha256:")
        );
        assert_eq!(
            artifact_recorder.read_immutable_content_artifact(&proposal.artifact_ref)?,
            proposal.artifact.content.as_bytes()
        );
    }
    assert!(
        outputs[0]
            .changeset_proposal
            .as_ref()
            .is_some_and(|proposal| proposal
                .change_set
                .files
                .iter()
                .any(|file| file.path == "worktree_a.txt"))
    );
    assert_eq!(
        outputs[0]
            .changeset_proposal
            .as_ref()
            .map(|proposal| proposal.change_set.files.len()),
        Some(1)
    );
    assert!(
        outputs[1]
            .changeset_proposal
            .as_ref()
            .is_some_and(|proposal| proposal
                .change_set
                .files
                .iter()
                .any(|file| file.path == "worktree_b.txt"))
    );
    assert_eq!(
        outputs[1]
            .changeset_proposal
            .as_ref()
            .map(|proposal| proposal.change_set.files.len()),
        Some(1)
    );
    assert_eq!(
        fs::read_to_string(repository_root.join("base.txt"))?,
        "user dirty baseline\n"
    );
    assert_eq!(
        fs::read_to_string(repository_root.join("user-notes.txt"))?,
        "safe untracked baseline\n"
    );
    assert!(!repository_root.join("worktree_a.txt").exists());
    assert!(!repository_root.join("worktree_b.txt").exists());
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(_))
            ))
            .count(),
        2
    );
    let prepared = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(prepared)) => {
                Some(prepared)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(prepared.iter().all(|entry| entry.overlay_entry_count == 2
        && entry.base_commit.is_some()
        && entry.overlay_digest == prepared[0].overlay_digest
        && entry.overlay_artifact_ref == prepared[0].overlay_artifact_ref));
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(_))
            ))
            .count(),
        2
    );
    let created = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(created)) => {
                Some(created)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(created.iter().all(|entry| entry.overlay_entry_count == 2
        && entry.materialized_snapshot_id.is_some()
        && entry.overlay_digest == prepared[0].overlay_digest
        && entry.overlay_artifact_ref == prepared[0].overlay_artifact_ref));
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCleanupRecorded(
                    cleanup
                )) if cleanup.status == IsolatedWorkspaceCleanupStatus::Removed
            ))
            .count(),
        2
    );
    assert!(
        session
            .write_isolation_projection()
            .isolated_workspace_cleanup_inventory()
            .is_empty()
    );
    assert!(
        !repository_root
            .join(".git/sigil-isolated-worktrees")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn worktree_child_writes_only_inside_bound_workspace_and_returns_review_artifact()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_repository_root = temp.path().join("source-repository");
    initialize_worktree_test_repository(&source_repository_root)?;
    let repository_root = temp.path().join("parent-worktree");
    let repository_root_text = repository_root
        .to_str()
        .ok_or_else(|| anyhow!("temporary worktree path should be UTF-8"))?;
    run_worktree_test_git(
        &source_repository_root,
        &["worktree", "add", "--detach", repository_root_text, "HEAD"],
    )?;
    let base_snapshot_id = worktree_test_snapshot_id(&repository_root)?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["write_file".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut write_tools = ToolRegistry::new();
    write_tools.register(Arc::new(WorktreeWriteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider {
                text: "read complete",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(Box::new(WorktreeWriteProvider), write_tools),
    );
    let logs_root = temp.path().join("logs");
    fs::create_dir(&logs_root)?;
    let store = JsonlSessionStore::new(logs_root.join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;
    let step = worktree_step("isolated_write")?;
    let output = runner
        .run_child_session(
            &mut session,
            TaskChildSessionRunRequest {
                task: sigil_kernel::SequentialTaskRequest {
                    task_id: TaskId::new("task_worktree")?,
                    parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
                    objective: "edit one file in a physical child worktree".to_owned(),
                },
                plan_version: 1,
                step: step.clone(),
                attempt_id: task_participant_attempt_id(
                    &TaskId::new("task_worktree")?,
                    TaskParticipantPurpose::Step,
                    Some(1),
                    Some(&step.step_id),
                    1,
                )?,
                child_session_ref: task_participant_session_ref(
                    &TaskId::new("task_worktree")?,
                    &task_participant_attempt_id(
                        &TaskId::new("task_worktree")?,
                        TaskParticipantPurpose::Step,
                        Some(1),
                        Some(&step.step_id),
                        1,
                    )?,
                )?,
                child_input: AgentRunInput::without_persisted_user_message(vec![
                    ModelMessage::user("write base.txt in the isolated worktree"),
                ]),
                options: run_options(repository_root.clone()),
                isolated_base_snapshot_id: Some(base_snapshot_id.clone()),
            },
            &mut handler,
            &mut approval,
        )
        .await?;

    assert_eq!(
        fs::read_to_string(repository_root.join("base.txt"))?,
        "base\n"
    );
    assert!(output.outcome.changed_files.is_empty());
    assert_eq!(
        output.isolated_parent_snapshot_id.as_deref(),
        Some(base_snapshot_id.as_str())
    );
    let proposal = output
        .changeset_proposal
        .expect("worktree child should return a review artifact");
    assert_eq!(proposal.source_isolation, WriteIsolationMode::Worktree);
    assert!(proposal.child_snapshot_id.is_some());
    assert!(
        proposal
            .change_set
            .files
            .iter()
            .any(|file| file.path == "base.txt")
    );
    assert!(proposal.artifact.content.contains("isolated edit"));
    assert!(
        proposal
            .artifact_ref
            .starts_with("mutation-artifact:sha256:")
    );
    let persisted_artifact = session
        .mutation_event_recorder()
        .expect("durable parent session should own a mutation artifact recorder")
        .read_immutable_content_artifact(&proposal.artifact_ref)?;
    assert_eq!(
        persisted_artifact,
        proposal.artifact.content.as_bytes(),
        "the durable review artifact must preserve the exact extracted diff"
    );
    assert_eq!(
        proposal.integration_facts.changeset_artifact_ref, proposal.artifact_ref,
        "integration facts must bind the durable artifact rather than the transient inline ref"
    );

    let prepared_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::IsolatedWorkspacePrepared(_))
            )
        })
        .expect("worktree preparation should be durable");
    let created_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCreated(_))
            )
        })
        .expect("worktree creation should be durable");
    let cleanup_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCleanupRecorded(
                    cleanup
                )) if cleanup.status == IsolatedWorkspaceCleanupStatus::Removed
            )
        })
        .expect("worktree cleanup should be durable");
    assert!(prepared_index < created_index);
    assert!(created_index < cleanup_index);
    assert!(
        session
            .write_isolation_projection()
            .isolated_workspace_cleanup_inventory()
            .is_empty()
    );
    assert!(
        !source_repository_root
            .join(".git/sigil-isolated-worktrees")
            .exists()
    );
    let thread = session
        .agent_thread_state_projection()
        .threads
        .values()
        .next()
        .expect("worktree child thread should be durable")
        .clone();
    let child_workspace = thread
        .run_context
        .as_ref()
        .map(|context| context.workspace_root.as_str())
        .expect("child run context should bind a workspace");
    assert!(child_workspace.contains("/.git/sigil-isolated-worktrees/worktree-"));
    assert_ne!(child_workspace, repository_root.display().to_string());
    assert!(!Path::new(child_workspace).exists());
    Ok(())
}

#[tokio::test]
async fn cancelled_worktree_child_still_records_terminal_cleanup() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let repository_root = temp.path().join("repository");
    initialize_worktree_test_repository(&repository_root)?;
    let base_snapshot_id = worktree_test_snapshot_id(&repository_root)?;
    let mut config = root_config();
    config.task.allow_write_subagents = true;
    config.task.subagent_write.tools = sigil_kernel::ToolAllowlistConfig {
        allow_all: false,
        names: vec!["write_file".to_owned()],
        prefixes: Vec::new(),
    };
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let mut write_tools = ToolRegistry::new();
    write_tools.register(Arc::new(WorktreeWriteTool));
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider {
                text: "read complete",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(Box::new(WorktreeWriteProvider), write_tools),
    );
    let logs_root = temp.path().join("logs");
    fs::create_dir(&logs_root)?;
    let store = JsonlSessionStore::new(logs_root.join("parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let step = worktree_step("cancelled_write")?;
    let attempt_id = task_participant_attempt_id(
        &TaskId::new("task_worktree_cancel")?,
        TaskParticipantPurpose::Step,
        Some(1),
        Some(&step.step_id),
        1,
    )?;
    let cancellation = RunCancellationOwner::new();
    assert!(cancellation.request_cancel());
    let request = TaskChildSessionRunRequest {
        task: sigil_kernel::SequentialTaskRequest {
            task_id: TaskId::new("task_worktree_cancel")?,
            parent_session_ref: SessionRef::new_relative("parent.jsonl")?,
            objective: "cancel after physical worktree preparation".to_owned(),
        },
        plan_version: 1,
        step,
        attempt_id: attempt_id.clone(),
        child_session_ref: task_participant_session_ref(
            &TaskId::new("task_worktree_cancel")?,
            &attempt_id,
        )?,
        child_input: AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
            "this run is already cancelled",
        )])
        .with_child_cancellation(cancellation.handle()),
        options: run_options(repository_root.clone()),
        isolated_base_snapshot_id: Some(base_snapshot_id),
    };
    let mut handler = RecordingEventHandler::default();
    let mut approval = AutoApproveHandler;

    let error = runner
        .run_child_session(&mut session, request, &mut handler, &mut approval)
        .await
        .expect_err("pre-cancelled worktree child should not dispatch");

    assert!(format!("{error:#}").contains("cancel"), "{error:#}");
    assert_eq!(
        fs::read_to_string(repository_root.join("base.txt"))?,
        "base\n"
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::IsolatedWorkspaceCleanupRecorded(cleanup))
            if cleanup.status == IsolatedWorkspaceCleanupStatus::Removed
    )));
    assert!(
        session
            .write_isolation_projection()
            .isolated_workspace_cleanup_inventory()
            .is_empty()
    );
    assert!(
        !repository_root
            .join(".git/sigil-isolated-worktrees")
            .exists()
    );
    Ok(())
}

#[test]
fn child_terminal_facts_treat_unclassified_shell_as_global() -> Result<()> {
    let mut child_session = Session::new("deepseek", "deepseek-v4-flash");
    child_session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: "call-shell".to_owned(),
        tool_name: "bash".to_owned(),
        status: ToolExecutionStatus::Completed,
        duration_ms: Some(1),
        subjects: Vec::new(),
        changed_files: vec!["src/lib.rs".to_owned()],
        metadata: ToolResultMeta::default(),
        error: None,
        model_content_hash: Some("sha256-shell-result".to_owned()),
    })))?;
    let mut proposal = decode_changeset_only_child_output(
        r#"{
            "change_set": {
                "id": "change-shell",
                "title": "shell change",
                "summary": "shell change",
                "risk": "medium",
                "files": [{
                    "path": "src/lib.rs",
                    "action": "update",
                    "risk": "medium",
                    "before_hash": "before",
                    "after_hash": "after",
                    "additions": 1,
                    "deletions": 1
                }],
                "validations": []
            },
            "artifact": {
                "media_type": "text/x-diff",
                "content": "--- a/src/lib.rs\n+++ b/src/lib.rs\n"
            }
        }"#,
    )?;

    bind_child_integration_facts(&child_session, &mut proposal)?;

    assert_eq!(
        proposal.integration_facts.declared_effect,
        IntegrationEffect::Global
    );
    assert!(
        proposal
            .integration_facts
            .observed_effects
            .contains(&IntegrationObservedEffect::UnknownShell)
    );
    Ok(())
}

#[tokio::test]
async fn task_runner_persists_acknowledged_integration_lane_lifecycle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let repository_root = temp.path().join("integration-parent");
    initialize_worktree_test_repository(&repository_root)?;
    let base_snapshot_id = worktree_test_snapshot_id(&repository_root)?;
    let base_commit_output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(&repository_root)
        .output()?;
    let base_commit = String::from_utf8(base_commit_output.stdout)?
        .trim()
        .to_owned();
    let before_hash = format!("{:x}", Sha256::digest(b"base\n"));
    let after_hash = format!("{:x}", Sha256::digest(b"integrated\n"));
    let change_set = ChangeSet {
        id: ChangeSetId::new("changeset-integration-lifecycle")?,
        title: "integration lifecycle".to_owned(),
        summary: "integration lifecycle".to_owned(),
        risk: ChangeSetRisk::Medium,
        files: vec![ChangeSetFile {
            path: "base.txt".to_owned(),
            previous_path: None,
            action: ChangeSetFileAction::Update,
            risk: ChangeSetRisk::Medium,
            before_hash: Some(before_hash),
            after_hash: Some(after_hash),
            diff_hash: None,
            additions: 1,
            deletions: 1,
            validations: Vec::new(),
        }],
        validations: Vec::new(),
    };
    let facts = IntegrationProposalFacts::from_changeset(
        &change_set,
        IntegrationBaseRepresentation::CleanCommit {
            base_commit: base_commit.clone(),
        },
        IntegrationContentClass::Text,
        IntegrationEffect::Files,
        Vec::new(),
        "inline:integration-lifecycle",
        Vec::new(),
    )?;
    let step_id = TaskStepId::new("step_integration_lifecycle")?;
    let plan = build_integration_plan(
        IntegrationPlanId::new("plan-integration-lifecycle")?,
        TaskId::new("task_integration_lifecycle")?,
        1,
        vec![IntegrationProposalSpec::from_changeset(
            &change_set,
            step_id.clone(),
            base_snapshot_id.clone(),
            Vec::new(),
            Vec::new(),
            IntegrationEffect::Files,
            DEFAULT_TASK_VERIFICATION_SCOPE_HASH,
            facts.clone(),
        )?],
    )?;
    let patch = "--- a/base.txt\n+++ b/base.txt\n@@ -1,1 +1,1 @@\n-base\n+integrated\n".to_owned();
    let proposal = TaskIntegrationProposal {
        step_id,
        depends_on: Vec::new(),
        base_snapshot_id: base_snapshot_id.clone(),
        proposal: TaskChildChangeSetProposal {
            change_set,
            artifact_ref: "inline:integration-lifecycle".to_owned(),
            artifact: TaskChildChangeSetArtifact {
                media_type: "text/x-diff".to_owned(),
                content_sha256: format!("{:x}", Sha256::digest(patch.as_bytes())),
                content: patch,
            },
            source_isolation: WriteIsolationMode::Worktree,
            child_snapshot_id: None,
            integration_facts: facts,
        },
    };
    let config = root_config();
    let supervisor = AgentSupervisor::new(
        AgentProfileRegistry::from_root_config(&config)?,
        AgentBudgetPolicy::from_root_config(&config),
        provider_capabilities(),
    );
    let runner = AgentSupervisorTaskChildRunner::new(
        supervisor,
        Agent::new(
            Box::new(TextProvider {
                text: "read complete",
            }),
            ToolRegistry::new(),
        ),
        Agent::new(
            Box::new(TextProvider {
                text: "write complete",
            }),
            ToolRegistry::new(),
        ),
    );
    let store = JsonlSessionStore::new(temp.path().join("integration-parent.jsonl"))?;
    let mut session = Session::load_from_store("deepseek", "deepseek-v4-flash", store.clone())?;
    session.append_control(ControlEntry::IntegrationPlanRecorded(
        sigil_kernel::IntegrationPlanRecorded { plan: plan.clone() },
    ))?;
    let mut handler = RecordingEventHandler::default();
    let output = runner
        .run_integration_lanes(
            &mut session,
            TaskIntegrationRunRequest {
                plan: plan.clone(),
                workspace_root: repository_root.clone(),
                proposals: vec![proposal],
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.lanes.len(), 1);
    assert_eq!(
        output.lanes[0].status,
        sigil_kernel::IntegrationLaneStatus::Ready
    );
    let preview = output
        .promotion_preview
        .as_ref()
        .expect("ready lanes should produce one exact promotion preview");
    assert_eq!(preview.plan_id, plan.plan_id);
    assert_eq!(
        preview.target,
        sigil_kernel::IntegrationPromotionTarget::WorkspaceApply {
            expected_snapshot_id: base_snapshot_id,
            expected_revision: 0,
        }
    );
    assert!(
        preview
            .aggregate_diff_artifact_ref
            .starts_with("mutation-artifact:sha256:")
    );
    let aggregate_diff = session
        .mutation_event_recorder()
        .expect("durable integration session should own an artifact recorder")
        .read_immutable_content_artifact(&preview.aggregate_diff_artifact_ref)?;
    assert_eq!(
        preview.aggregate_diff_digest,
        format!("sha256:{:x}", Sha256::digest(&aggregate_diff))
    );
    assert!(String::from_utf8_lossy(&aggregate_diff).contains("+integrated"));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::IntegrationLanePrepared(_))
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::IntegrationLaneMemberApplied(_))
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::IntegrationLaneVerificationLinked(_))
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::IntegrationLaneTerminal(_))
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::IntegrationLaneCleanupRecorded(_))
    )));
    let projection = sigil_kernel::IntegrationProjection::from_entries(session.entries());
    let lifecycle = projection
        .latest()
        .and_then(|state| state.lifecycle_lanes.values().next())
        .expect("replayed integration lifecycle");
    assert!(!lifecycle.inconsistent);
    assert_eq!(
        lifecycle.terminal.as_ref().map(|entry| entry.status),
        Some(sigil_kernel::IntegrationLaneStatus::Ready)
    );
    let verification = lifecycle
        .verification
        .as_ref()
        .expect("lane verification receipt link");
    assert_eq!(verification.verification_receipts.len(), 1);
    let receipt = &verification.verification_receipts[0];
    assert_eq!(
        receipt.binding.execution_backend,
        Some(sigil_kernel::ExecutionBackendKind::Local)
    );
    assert_eq!(
        receipt.binding.execution_network.policy,
        sigil_kernel::ExecutionNetworkPolicy::Unknown
    );
    assert_eq!(
        receipt.binding.verification_scope_hash,
        DEFAULT_TASK_VERIFICATION_SCOPE_HASH
    );
    assert_eq!(fs::read(repository_root.join("base.txt"))?, b"base\n");
    session.append_control(ControlEntry::TaskPromotionPreviewRecorded(
        sigil_kernel::TaskPromotionPreviewRecorded {
            preview: preview.clone(),
        },
    ))?;
    drop(session);
    let restored = Session::load_from_store("deepseek", "deepseek-v4-flash", store)?;
    let restored_projection = sigil_kernel::IntegrationProjection::from_entries(restored.entries());
    let restored_preview = restored_projection
        .plans
        .get(&plan.plan_id)
        .and_then(|state| state.promotion_previews.get(&preview.preview_digest))
        .expect("promotion preview should survive parent session ownership transfer");
    assert_eq!(restored_preview, preview);
    assert_eq!(
        restored
            .mutation_event_recorder()
            .expect("restored session should retain its artifact recorder")
            .read_immutable_content_artifact(&restored_preview.aggregate_diff_artifact_ref)?,
        aggregate_diff
    );
    Ok(())
}

fn initialize_worktree_test_repository(repository_root: &Path) -> Result<()> {
    fs::create_dir(repository_root)?;
    run_worktree_test_git(repository_root, &["init", "--quiet"])?;
    run_worktree_test_git(
        repository_root,
        &["config", "user.name", "Sigil Runtime Tests"],
    )?;
    run_worktree_test_git(
        repository_root,
        &[
            "config",
            "user.email",
            "sigil-runtime-tests@example.invalid",
        ],
    )?;
    fs::write(repository_root.join("base.txt"), "base\n")?;
    run_worktree_test_git(repository_root, &["add", "base.txt"])?;
    run_worktree_test_git(repository_root, &["commit", "--quiet", "-m", "base"])?;
    Ok(())
}

fn worktree_test_snapshot_id(repository_root: &Path) -> Result<String> {
    let workspace_id = stable_workspace_id(repository_root)?;
    build_workspace_snapshot(
        repository_root,
        workspace_id,
        &VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH),
        0,
    )?
    .workspace_snapshot_id
    .ok_or_else(|| anyhow!("worktree test snapshot should be complete"))
}

fn run_worktree_test_git(repository_root: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repository_root)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(())
}
