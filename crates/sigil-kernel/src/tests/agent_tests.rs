use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::OpenOptions,
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
use fs2::FileExt;
use futures::{Stream, stream};
use serde_json::{Value, json};

#[cfg(unix)]
use std::os::unix::fs::symlink;

use crate::session::SessionWriterFault;
use crate::{
    AgentRole, AgentRunDisposition, AgentRunPurpose, ApprovalHandler, ApprovalMode,
    AssistantMessageKind, AutoApproveHandler, AutomaticRouteCapability, BackgroundTaskHandle,
    BackgroundTaskStatus, CONTINUE_EXISTING_TASK_TOOL_NAME,
    CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME, CompactionConfig, CompletionRequest, ContextBodyRef,
    ContextInclusionReason, ContextItem, ContextSensitivity, ContextSource, ContextTrustLevel,
    ControlEntry, ConversationInputQueueId, ConversationPurposeContext, ConversationRoute,
    ConversationRouteReason, ConversationTurnRef, DurableEventType, EventHandler,
    ExternalDirectoryConfig, ExternalDirectoryRule, ExternalEvidenceLevel, ExternalSourceRecord,
    FrozenProviderRequestMaterial, InteractionMode, JsonlSessionStore, MemoryConfig, MessageRole,
    ModelMessage, MutationEventRecorder, PermissionConfig, PermissionDecision, PlanApprovalExpiry,
    PlanApprovalPermission, PlanApprovalScope, PlanId, PlanPermissionGrantedEntry,
    PlanReviewHandoffBinding, PlanReviewPurposeContext, PreparedToolExecution,
    PromotedConversationInput, Provider, ProviderCapabilities, ProviderChunk,
    ProviderContinuationState, ProviderFailureClassV1, ProviderFailureObservationV1,
    ProviderPhysicalAttemptOutcome, ProviderPhysicalAttemptProjection,
    ProviderPhysicalAttemptStartedEntry, ProviderPhysicalAttemptTerminalEntry,
    ProviderRequestRejection, ProviderTurnRecoveryEvidenceV1, ProviderTurnRecoveryPolicyV1,
    ProviderWireStateV1, REQUEST_PLAN_REVIEW_TOOL_NAME, REQUEST_TASK_PLANNING_TOOL_NAME,
    REQUEST_USER_INPUT_TOOL_NAME, ReasoningArtifact, ReasoningEffort, ReasoningStreamSupport,
    ResponseHandle, RunCancellationOwner, RunEvent, RuntimeContextCandidates,
    SUBMIT_PLAN_DRAFT_TOOL_NAME, SecretString, Session, SessionLogEntry, SessionRef,
    SessionStreamRecord, SourceCacheStatus, SourceFreshness, TASK_GUIDANCE_APPLY_TOOL_NAME,
    TASK_PLAN_UPDATE_TOOL_NAME, TOOL_ARTIFACT_READ_SCHEMA_VERSION, TaskContinuationHandoffBinding,
    TaskGuidanceApplyReason, TaskGuidanceAssessmentContext, TaskHandoffId, TaskId,
    TaskParticipantAttemptId, TaskParticipantContext, TaskPlanEntry, TaskPlanStatus,
    TaskPlanUpdateContext, TaskPlannerWorktreeAvailability, TaskPlanningHandoffBinding,
    TaskRoutingPolicy, TaskRunEntry, TaskRunStatus, TaskStepId, TaskStepSpec, TerminalTaskStatus,
    Tool, ToolAccess, ToolApproval, ToolApprovalAllowSource, ToolApprovalAuditAction,
    ToolApprovalUserDecision, ToolArtifactReadOutcome, ToolArtifactReadRecordedV1,
    ToolArtifactRefV1, ToolArtifactSelectorV1, ToolCall, ToolCategory, ToolConcurrencyClass,
    ToolContext, ToolEgressAudit, ToolErrorKind, ToolExecutionId, ToolExecutionStatus,
    ToolMutationTracking, ToolPreparation, ToolPreview, ToolPreviewCapability, ToolPreviewFile,
    ToolProgressEvent, ToolRegistry, ToolRestartPolicy, ToolResult, ToolResultMeta, ToolSubject,
    ToolSubjectScope, UsageStats, UserUrlCapabilityRegistrar, UserUrlCapabilityRegistration,
    VerificationVerdict, VisibleCompletionState, WebUrlProvenanceKind, WorkspaceMutationDetected,
    conversation_route_decision_id_for_source, conversation_route_routing_contract_material,
    direct_conversation_continuation_prompt_contract_material, plan_review_attempt_id_for_review,
    plan_review_id_for_source, plan_review_plan_id_for_attempt, plan_review_policy_snapshot_hash,
    plan_text_hash, route_surface_tool_specs,
    task_participant_finalization_prompt_contract_material,
    task_participant_system_prompt_contract_material,
};

use super::{
    Agent, AgentDelegationRequirement, AgentRunInput, AgentRunOptions, AgentRunOutcome,
    AgentRunTerminalReason, AgentToolDelegate, FinalAnswerContext,
    PendingConversationInputProvider, TASK_PARTICIPANT_POST_MUTATION_READ_TAIL_LIMIT,
    build_task_step_checkpoint, emit_tool_result,
};

/// Host-shaped plan review binding for routing tests; identity is derived from the source turn.
fn test_plan_review_handoff_binding(
    source_turn: &ConversationTurnRef,
    objective: &str,
) -> PlanReviewHandoffBinding {
    let plan_review_id = plan_review_id_for_source(source_turn);
    let attempt_id = plan_review_attempt_id_for_review(&plan_review_id);
    let plan_id = plan_review_plan_id_for_attempt(&plan_review_id, &attempt_id);
    PlanReviewHandoffBinding {
        decision_id: conversation_route_decision_id_for_source(source_turn),
        plan_review_id,
        attempt_id,
        plan_id,
        source_turn: source_turn.clone(),
        objective: objective.to_owned(),
        policy_snapshot_hash: plan_review_policy_snapshot_hash(),
        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
        pending_plan: None,
        requested_at_ms: 42,
        decided_at_ms: 43,
    }
}

#[test]

fn emitting_small_tool_results_keeps_later_model_previews_visible() -> Result<()> {
    let mut session = Session::new("test", "model");
    let mut handler = RecordingEventHandler::default();

    for index in 0..12 {
        let tool_name = if index % 2 == 0 { "read_file" } else { "shell" };
        let body = format!("small result {index}");
        emit_tool_result(
            &mut session,
            &mut handler,
            ToolResult::ok(
                format!("call-small-{index}"),
                tool_name,
                body.clone(),
                ToolResultMeta::default(),
            ),
        )?;
        assert!(matches!(
            session.entries().last(),
            Some(SessionLogEntry::ToolResultV3(result))
                if result.initial_model_view.preview == body
        ));
    }

    Ok(())
}

fn declared_test_permission_plan<T: Tool + ?Sized>(
    tool: &T,
    args: &Value,
    subjects: Vec<ToolSubject>,
    tool_default_mode: Option<ApprovalMode>,
) -> Result<crate::ToolPermissionPlanDraft> {
    let spec = tool.spec();
    let access = spec.access;
    crate::declared_tool_permission_plan(
        &spec,
        args,
        crate::DeclaredToolPermissionFacts {
            access,
            operation: crate::infer_tool_operation(&spec.name, access),
            network_effect: spec.network_effect,
            subjects,
            tool_default_mode,
        },
    )
}

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

struct CapturingRoutingProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

struct PlanReviewRoutingProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

struct PlanReviewWithMemoryRoutingProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

struct ChatDecisionRoutingProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for PlanReviewRoutingProvider {
    fn name(&self) -> &str {
        "mock-plan-review-routing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        let args = r#"{"reason_codes":["architectural_tradeoff","scope_uncertain"]}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-plan-review-1".to_owned(),
                name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-plan-review-1".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-plan-review-1".to_owned(),
                name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for PlanReviewWithMemoryRoutingProvider {
    fn name(&self) -> &str {
        "mock-plan-review-with-memory-routing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        let memory_args = r#"{"statement":"When I say finish, create the commit."}"#;
        let route_args = r#"{"reason_codes":["architectural_tradeoff"]}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ReasoningDelta(
                "memory routing reasoning stays internal".to_owned(),
            )),
            Ok(ProviderChunk::TextDelta(
                "memory routing narrative stays internal".to_owned(),
            )),
            Ok(ProviderChunk::ToolCallStart {
                id: "call-plan-review-with-memory".to_owned(),
                name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-plan-review-with-memory".to_owned(),
                delta: route_args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-plan-review-with-memory".to_owned(),
                name: crate::REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                args_json: route_args.to_owned(),
            })),
            Ok(ProviderChunk::ToolCallStart {
                id: "call-remember-routing".to_owned(),
                name: crate::REMEMBER_USER_PREFERENCE_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-remember-routing".to_owned(),
                delta: memory_args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-remember-routing".to_owned(),
                name: crate::REMEMBER_USER_PREFERENCE_TOOL_NAME.to_owned(),
                args_json: memory_args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ChatDecisionRoutingProvider {
    fn name(&self) -> &str {
        "mock-chat-decision-routing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let is_routing_microturn = request
            .tools
            .iter()
            .any(|tool| tool.name == crate::REQUEST_PLAN_REVIEW_TOOL_NAME);
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        if is_routing_microturn {
            let args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ReasoningDelta(
                    "internal routing reasoning".to_owned(),
                )),
                Ok(ProviderChunk::TextDelta(
                    "internal routing narrative".to_owned(),
                )),
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-chat-decision".to_owned(),
                    name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-chat-decision".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-chat-decision".to_owned(),
                    name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ReasoningDelta(
                "work reasoning is visible".to_owned(),
            )),
            Ok(ProviderChunk::TextDelta(
                "queue promotion is a durable CAS promotion".to_owned(),
            )),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct LateTaskHandoffProvider {
    calls: Arc<AtomicUsize>,
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

#[async_trait]
impl Provider for CapturingRoutingProvider {
    fn name(&self) -> &str {
        "mock-capturing-routing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        CapturingTextProvider {
            captured: Arc::new(Mutex::new(Vec::new())),
        }
        .capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let routing_microturn = request
            .tools
            .iter()
            .any(|tool| tool.name == REQUEST_TASK_PLANNING_TOOL_NAME);
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        if routing_microturn {
            let args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-routing-side-effect".to_owned(),
                    name: "handoff_side_effect".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-routing-side-effect".to_owned(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-routing-side-effect".to_owned(),
                    name: "handoff_side_effect".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-continue-routing".to_owned(),
                    name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-continue-routing".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-continue-routing".to_owned(),
                    name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("captured".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for LateTaskHandoffProvider {
    fn name(&self) -> &str {
        "mock-late-task-handoff"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        CapturingTextProvider {
            captured: Arc::new(Mutex::new(Vec::new())),
        }
        .capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        if turn == 0 {
            let args = r#"{"reason":"does_not_meet_task_planning_criteria"}"#;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-continue-routing".to_owned(),
                    name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-continue-routing".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-continue-routing".to_owned(),
                    name: CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        if turn == 1 {
            let args = r#"{"reason_codes":["cross_layer"]}"#;
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-late-handoff".to_owned(),
                    name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-late-handoff".to_owned(),
                    delta: args.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-late-handoff".to_owned(),
                    name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
                    args_json: args.to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("ordinary answer".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct DegradingRoutingProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for DegradingRoutingProvider {
    fn name(&self) -> &str {
        "mock-degrading-routing"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        CapturingTextProvider {
            captured: Arc::new(Mutex::new(Vec::new())),
        }
        .capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        if turn < 2 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("captured".to_owned())),
                Ok(ProviderChunk::ToolCallStart {
                    id: "call-invalid-routing-tool".to_owned(),
                    name: "handoff_side_effect".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "call-invalid-routing-tool".to_owned(),
                    delta: "{}".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-invalid-routing-tool".to_owned(),
                    name: "handoff_side_effect".to_owned(),
                    args_json: "{}".to_owned(),
                })),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("final answer".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct ToolSideEffectProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}
#[derive(Default)]
struct ToolRunFactsDelegate {
    root_logical_run_id: Option<String>,
}
struct EvolvingToolFactsProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
    turns: AtomicUsize,
}
#[derive(Default)]
struct EvolvingToolRunFactsDelegate;
#[derive(Default)]
struct SettlingToolRunFactsDelegate;
struct SequencedFinalAnswerBlockerDelegate {
    blockers: VecDeque<Option<String>>,
    fallback: Option<String>,
}
struct ForegroundTerminalProvider {
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
    tool_completed: Arc<AtomicBool>,
}
struct WorkspaceMutationToolProvider;
struct PostMutationReadLoopProvider {
    calls: Arc<AtomicUsize>,
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}
struct RepeatedReadLoopProvider {
    calls: Arc<AtomicUsize>,
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
    finalization_text: Option<String>,
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

#[async_trait]
impl Provider for EvolvingToolFactsProvider {
    fn name(&self) -> &str {
        "mock-evolving-tool-facts"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        let turn = self.turns.fetch_add(1, Ordering::SeqCst);
        if turn >= 2 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("done".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        }
        let call_id = format!("call-side-effect-{turn}");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: "side_effect".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: "{}".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: "side_effect".to_owned(),
                args_json: "{}".to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl AgentToolDelegate for ToolRunFactsDelegate {
    fn set_root_logical_run_id(&mut self, logical_run_id: Option<&str>) {
        self.root_logical_run_id = logical_run_id.map(str::to_owned);
    }

    async fn handle_agent_tool_call(
        &mut self,
        _session: &mut Session,
        _call: &ToolCall,
        _options: &AgentRunOptions,
        _handler: &mut (dyn EventHandler + Send),
        _approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        Ok(None)
    }

    fn final_answer_context(
        &mut self,
        _session: &Session,
        _options: &AgentRunOptions,
        outcome: &AgentRunOutcome,
    ) -> Result<Option<FinalAnswerContext>> {
        Ok(
            (!outcome.tool_call_ids.is_empty()).then(|| FinalAnswerContext {
                key: "tool-run-facts-v1".to_owned(),
                prompt: "recorded current-run facts".to_owned(),
            }),
        )
    }
}

#[async_trait]
impl AgentToolDelegate for EvolvingToolRunFactsDelegate {
    async fn handle_agent_tool_call(
        &mut self,
        _session: &mut Session,
        _call: &ToolCall,
        _options: &AgentRunOptions,
        _handler: &mut (dyn EventHandler + Send),
        _approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        Ok(None)
    }

    fn final_answer_context(
        &mut self,
        _session: &Session,
        _options: &AgentRunOptions,
        outcome: &AgentRunOutcome,
    ) -> Result<Option<FinalAnswerContext>> {
        let generation = outcome.tool_call_ids.len();
        Ok((generation > 0).then(|| FinalAnswerContext {
            key: format!("tool-run-facts-v{generation}"),
            prompt: format!("active run facts generation {generation}"),
        }))
    }
}

#[async_trait]
impl AgentToolDelegate for SettlingToolRunFactsDelegate {
    async fn handle_agent_tool_call(
        &mut self,
        _session: &mut Session,
        _call: &ToolCall,
        _options: &AgentRunOptions,
        _handler: &mut (dyn EventHandler + Send),
        _approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        Ok(None)
    }

    fn final_answer_context(
        &mut self,
        _session: &Session,
        _options: &AgentRunOptions,
        outcome: &AgentRunOutcome,
    ) -> Result<Option<FinalAnswerContext>> {
        Ok(
            (outcome.tool_call_ids.len() == 1).then(|| FinalAnswerContext {
                key: "tool-run-facts-active".to_owned(),
                prompt: "active run still has unsettled work".to_owned(),
            }),
        )
    }
}

#[async_trait]
impl AgentToolDelegate for SequencedFinalAnswerBlockerDelegate {
    async fn handle_agent_tool_call(
        &mut self,
        _session: &mut Session,
        _call: &ToolCall,
        _options: &AgentRunOptions,
        _handler: &mut (dyn EventHandler + Send),
        _approval_handler: &mut (dyn ApprovalHandler + Send),
    ) -> Result<Option<ToolResult>> {
        Ok(None)
    }

    fn final_answer_blocker(&mut self, _session: &mut Session) -> Result<Option<String>> {
        Ok(self
            .blockers
            .pop_front()
            .unwrap_or_else(|| self.fallback.clone()))
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

#[async_trait]
impl Provider for PostMutationReadLoopProvider {
    fn name(&self) -> &str {
        "mock-post-mutation-read-loop"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let tools_disabled = request.tools.is_empty();
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        if tools_disabled {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "mutation complete after bounded inspection".to_owned(),
                )),
                Ok(ProviderChunk::Done),
            ])));
        }

        let (name, args_json) = if call_index == 0 {
            ("workspace_mutation", "{}")
        } else {
            ("echo", r#"{"value":"inspect again"}"#)
        };
        let call_id = format!("call-post-mutation-{call_index}");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: name.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: args_json.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: name.to_owned(),
                args_json: args_json.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for RepeatedReadLoopProvider {
    fn name(&self) -> &str {
        "mock-repeated-read-loop"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let tools_disabled = request.tools.is_empty();
        self.captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .push(request);
        if tools_disabled {
            let mut chunks = Vec::new();
            if let Some(text) = self.finalization_text.as_deref() {
                if !text.is_empty() {
                    chunks.push(Ok(ProviderChunk::TextDelta(text.to_owned())));
                }
            } else {
                chunks.push(Ok(ProviderChunk::TextDelta(
                    "bounded result after repeated analysis".to_owned(),
                )));
            }
            chunks.push(Ok(ProviderChunk::Done));
            return Ok(Box::pin(stream::iter(chunks)));
        }
        let call_id = format!("call-repeated-read-{call_index}");
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: call_id.clone(),
                name: "echo".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: call_id.clone(),
                delta: r#"{"value":"same semantic read"}"#.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: call_id,
                name: "echo".to_owned(),
                args_json: r#"{"value":"same semantic read"}"#.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct EchoTool;
struct ProgressEchoTool;
#[derive(Default)]
struct ToolSchedulerProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
    events: Mutex<Vec<String>>,
}

struct ScheduledReadTool {
    name: String,
    delay: Duration,
    parallel: bool,
    mutation_tracking: ToolMutationTracking,
    fail: bool,
    probe: Arc<ToolSchedulerProbe>,
}

struct ForegroundTerminalTool {
    completed: Arc<AtomicBool>,
}
struct AgentCategoryTool;
struct FailingAgentCategoryTool;
struct RunningSpawnAgentCategoryTool;
struct RunningAgentCategoryTool;
struct SideEffectTool;
struct RoutingMemoryTool {
    executed: Arc<AtomicBool>,
    project_scoped: bool,
}
struct TerminalStartAuditTool;
struct TerminalCancelAuditTool;
struct WorkspaceMutatingCustomTool;
struct RecorderAwareEchoTool {
    saw_recorder: Arc<AtomicBool>,
    route_identity: Arc<Mutex<Option<(String, String)>>>,
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

fn synthetic_external_test_root() -> Result<PathBuf> {
    let current_dir = std::env::current_dir()?;
    let filesystem_root = current_dir
        .ancestors()
        .last()
        .ok_or_else(|| std::io::Error::other("current directory has no filesystem root"))?;
    Ok(std::fs::canonicalize(filesystem_root)?.join("sigil-test-external"))
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
impl Tool for ScheduledReadTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: self.name.clone(),
            description: "scheduler test read".to_owned(),
            input_schema: serde_json::json!({"type": "object"}),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        self.mutation_tracking
    }

    fn concurrency_class(&self) -> ToolConcurrencyClass {
        if self.parallel {
            ToolConcurrencyClass::ParallelReadOnly
        } else {
            ToolConcurrencyClass::Exclusive
        }
    }

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, Vec::new(), None)
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        let active = self.probe.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.probe.peak.fetch_max(active, Ordering::SeqCst);
        self.probe
            .events
            .lock()
            .expect("scheduler events lock should not be poisoned")
            .push(format!("start:{}", self.name));
        tokio::time::sleep(self.delay).await;
        self.probe
            .events
            .lock()
            .expect("scheduler events lock should not be poisoned")
            .push(format!("end:{}", self.name));
        self.probe.active.fetch_sub(1, Ordering::SeqCst);
        if self.fail {
            anyhow::bail!("scheduled read failed")
        }
        Ok(ToolResult::ok(
            call_id,
            self.name.clone(),
            self.name.clone(),
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
        *self
            .route_identity
            .lock()
            .expect("route identity lock should not be poisoned") = ctx
            .session_scope_id()
            .zip(ctx.logical_run_id())
            .map(|(session, run)| (session.to_owned(), run.to_owned()));
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        Ok(crate::ToolPermissionPlanDraft {
            access: ToolAccess::Read,
            operation: crate::ToolOperation::Read,
            effects: BTreeSet::from([crate::ToolPermissionEffect::FileRead]),
            subjects: vec![ToolSubject::path(path, path)],
            analysis: crate::ToolAnalysisStatus::Complete,
            containment: crate::ExecutionContainmentRequest {
                filesystem: crate::FilesystemContainment::WorkspaceReadOnly,
                process: crate::ProcessContainment::OwnedTree,
                environment: crate::EnvironmentContainment::Restricted,
                ..crate::ExecutionContainmentRequest::default()
            },
            semantic_scope: Some(crate::ToolSemanticScope::new("workspace_read", 1)),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([
                ("containment_proven".to_owned(), "true".to_owned()),
                (
                    "execution_backend".to_owned(),
                    "test-owned-process".to_owned(),
                ),
                ("execution_profile".to_owned(), "workspace-read".to_owned()),
                (
                    "environment_binding".to_owned(),
                    "test-restricted-v1".to_owned(),
                ),
            ]),
            safe_summary: crate::ToolPermissionSummary {
                title: "Read workspace path".to_owned(),
                detail: path.to_owned(),
                ..crate::ToolPermissionSummary::default()
            },
        })
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let command = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing command"))?;
        Ok(crate::ToolPermissionPlanDraft {
            access: ToolAccess::Execute,
            operation: crate::ToolOperation::ExecuteWorkspaceCheckCommand,
            effects: BTreeSet::from([
                crate::ToolPermissionEffect::FileRead,
                crate::ToolPermissionEffect::ExecuteWorkspaceCode,
            ]),
            subjects: vec![ToolSubject::command(command, "family:cargo_check")],
            analysis: crate::ToolAnalysisStatus::Complete,
            containment: crate::ExecutionContainmentRequest {
                filesystem: crate::FilesystemContainment::WorkspaceAndScratch,
                network: crate::NetworkContainment::Deny,
                process: crate::ProcessContainment::OwnedTree,
                environment: crate::EnvironmentContainment::Restricted,
                persistent_process: false,
            },
            semantic_scope: Some(crate::ToolSemanticScope::new("workspace_validation", 1)),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([
                ("containment_proven".to_owned(), "true".to_owned()),
                (
                    "execution_backend".to_owned(),
                    "test-owned-process".to_owned(),
                ),
                ("execution_profile".to_owned(), "build-offline".to_owned()),
                (
                    "environment_binding".to_owned(),
                    "test-restricted-v1".to_owned(),
                ),
            ]),
            safe_summary: crate::ToolPermissionSummary {
                title: "Run cargo check".to_owned(),
                detail: "workspace validation".to_owned(),
                step_count: 1,
                workspace_code_steps: 1,
            },
        })
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
impl Tool for RoutingMemoryTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::remember_memory_tool_spec(self.project_scoped)
    }

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, Vec::new(), Some(ApprovalMode::Ask))
    }

    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        Ok(Some(ToolPreview {
            title: "Remember user preference".to_owned(),
            summary: "Persist after approval".to_owned(),
            body: args["statement"].as_str().unwrap_or_default().to_owned(),
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        }))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        self.executed.store(true, Ordering::SeqCst);
        let tool_name = if self.project_scoped {
            crate::REMEMBER_PROJECT_FACT_TOOL_NAME
        } else {
            crate::REMEMBER_USER_PREFERENCE_TOOL_NAME
        };
        Ok(ToolResult::ok(
            call_id,
            tool_name,
            r#"{"receipt_type":"durable_memory_receipt_v1","durable":true,"memory_id":"memory-test","version":1}"#,
            ToolResultMeta::default(),
        ))
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, Vec::new(), Some(ApprovalMode::Allow))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, _args: Value) -> Result<ToolResult> {
        std::fs::write(ctx.workspace_root.join("mutated.txt"), "new\n")?;
        Ok(ToolResult::ok(
            call_id,
            "workspace_mutation",
            "mutated workspace",
            ToolResultMeta {
                changed_files: vec!["mutated.txt".to_owned()],
                ..ToolResultMeta::default()
            },
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, Vec::new(), Some(ApprovalMode::Allow))
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
                    "schema_version": crate::TERMINAL_TASK_SCHEMA_VERSION,
                    "task_id": "terminal-1",
                    "generation": 1,
                    "status": "running",
                    "status_detail": { "state": "running" },
                    "readiness": { "state": "none" },
                    "command_sha256": "0".repeat(64),
                    "cwd_label": ".",
                    "shell_label": "sh",
                    "shell_sha256": "1".repeat(64),
                    "log_ref": "terminal-log:terminal-1",
                    "created_at_ms": 10,
                    "updated_at_ms": 20,
                    "output_preview": "running output",
                    "output_hash": "2".repeat(64),
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, Vec::new(), Some(ApprovalMode::Allow))
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
                    "schema_version": crate::TERMINAL_TASK_SCHEMA_VERSION,
                    "task_id": "terminal-1",
                    "generation": 2,
                    "status": "cancelled",
                    "status_detail": { "state": "cancelled" },
                    "readiness": { "state": "none" },
                    "command_sha256": "0".repeat(64),
                    "cwd_label": ".",
                    "shell_label": "sh",
                    "shell_sha256": "1".repeat(64),
                    "log_ref": "terminal-log:terminal-1",
                    "created_at_ms": 10,
                    "updated_at_ms": 30,
                    "output_preview": "cancelled output",
                    "output_hash": crate::stable_event_hash(b"cancelled output"),
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        declared_test_permission_plan(self, args, vec![ToolSubject::path(path, path)], None)
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
        let subjects = self.permission_plan(&ctx, &args)?.subjects;
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

#[cfg(unix)]
struct SymlinkWriteTool {
    executed: Arc<AtomicBool>,
}

#[cfg(unix)]
#[async_trait]
impl Tool for SymlinkWriteTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "write_file".to_owned(),
            description: "write through a workspace symlink".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn permission_plan(
        &self,
        ctx: &ToolContext,
        args: &Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        let workspace = ctx.workspace_root.canonicalize()?;
        let canonical = workspace.join(path).canonicalize()?;
        let scope = if canonical.starts_with(&workspace) {
            ToolSubjectScope::Workspace
        } else {
            ToolSubjectScope::External
        };
        declared_test_permission_plan(
            self,
            args,
            vec![ToolSubject::path_with_scope(
                path,
                path,
                Some(canonical),
                scope,
            )],
            None,
        )
    }

    async fn preview(&self, _ctx: ToolContext, _args: Value) -> Result<Option<ToolPreview>> {
        Ok(Some(ToolPreview {
            title: "Write file".to_owned(),
            summary: "Write one workspace path".to_owned(),
            body: "write file.txt".to_owned(),
            changed_files: vec!["file.txt".to_owned()],
            file_diffs: Vec::new(),
        }))
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        _args: Value,
    ) -> Result<ToolResult> {
        self.executed.store(true, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "write_file",
            "unexpected write",
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        declared_test_permission_plan(
            self,
            args,
            vec![ToolSubject::path(path, path)],
            Some(ApprovalMode::Allow),
        )
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(
            self,
            args,
            vec![ToolSubject::path_with_scope(
                self.external_path.display().to_string(),
                self.external_path.display().to_string(),
                Some(self.external_path.clone()),
                ToolSubjectScope::External,
            )],
            None,
        )
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

struct ExpiredApprovalHandler;

struct ApproveForSessionHandler {
    approvals: Arc<AtomicUsize>,
}

struct ApproveWithArgsHandler;

struct PanicApprovalHandler;

#[cfg(unix)]
struct RetargetSymlinkApprovalHandler {
    link: PathBuf,
    protected_target: PathBuf,
}

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

impl ApprovalHandler for ExpiredApprovalHandler {
    fn approve_tool_call(
        &mut self,
        _call: &ToolCall,
        _spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        Ok(ToolApproval::Expired {
            reason: "approval request expired before a decision".to_owned(),
        })
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
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

    fn approve_tool_call_with_context(
        &mut self,
        call: &ToolCall,
        spec: &crate::ToolSpec,
        context: &crate::ToolApprovalContext,
    ) -> Result<ToolApproval> {
        assert_eq!(context.expires_at_ms, crate::APPROVAL_REQUEST_NO_EXPIRY_MS);
        assert_eq!(
            context.identity.expires_at_ms,
            crate::APPROVAL_REQUEST_NO_EXPIRY_MS
        );
        self.approve_tool_call(call, spec)
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

#[cfg(unix)]
impl ApprovalHandler for RetargetSymlinkApprovalHandler {
    fn approve_tool_call(
        &mut self,
        _call: &ToolCall,
        _spec: &crate::ToolSpec,
    ) -> Result<ToolApproval> {
        std::fs::remove_file(&self.link)?;
        symlink(&self.protected_target, &self.link)?;
        Ok(ToolApproval::Approve)
    }

    fn approval_is_explicit_user_action(&self) -> bool {
        true
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

struct SessionReadLockingEventHandler {
    session_path: PathBuf,
    shared_lock: Option<std::fs::File>,
    events: Vec<RunEvent>,
}

impl EventHandler for SessionReadLockingEventHandler {
    fn handle(&mut self, event: RunEvent) -> Result<()> {
        if self.shared_lock.is_none()
            && matches!(
                &event,
                RunEvent::Control(ControlEntry::WebUrlCapabilityDescriptor(_))
            )
        {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(&self.session_path)?;
            FileExt::lock_shared(&file)?;
            self.shared_lock = Some(file);
        }
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        _args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        anyhow::bail!("access exploded");
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        _call_id: String,
        _args: serde_json::Value,
    ) -> Result<ToolResult> {
        unreachable!("tool should not execute when permission_plan fails")
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        declared_test_permission_plan(self, args, vec![ToolSubject::path(path, path)], None)
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        declared_test_permission_plan(self, args, vec![ToolSubject::path(path, path)], None)
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing string field path"))?;
        declared_test_permission_plan(
            self,
            args,
            vec![ToolSubject::path(path, path)],
            Some(ApprovalMode::Allow),
        )
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
        .run_with_input(
            &mut session,
            AgentRunInput::user("hi"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?
        .result;
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
                content.contains(r#""preview":"hello""#)
                    && content.contains(r#""preview_kind":"complete""#)
            })
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(decision))
                if decision.call_id == "call-1"
                    && decision.policy_decision == ApprovalMode::Allow
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
async fn final_answer_context_is_injected_before_the_post_tool_provider_request() -> Result<()> {
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
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let mut delegate = ToolRunFactsDelegate::default();

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("load facts"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
            &mut delegate,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    let captured = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(
        captured.len(),
        2,
        "one tool request and one final-answer request should be sufficient"
    );
    assert!(captured[1].messages.iter().any(|message| {
        message.role == MessageRole::User
            && message.content.as_deref() == Some("recorded current-run facts")
    }));
    assert!(!handler.events.iter().any(|event| {
        matches!(event, RunEvent::Notice(message) if message.contains("recorded run facts added before final answer"))
    }));
    Ok(())
}

#[tokio::test]
async fn evolving_final_answer_context_replaces_the_prior_snapshot() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(SideEffectTool));
    let agent = Agent::new(
        EvolvingToolFactsProvider {
            captured: Arc::clone(&captured),
            turns: AtomicUsize::new(0),
        },
        registry,
    );
    let mut session = Session::new("mock-evolving-tool-facts", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let mut delegate = EvolvingToolRunFactsDelegate;

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("load evolving facts"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(5),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
            &mut delegate,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    let captured = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(captured.len(), 3);
    let final_request = captured.last().expect("final request should be captured");
    let fact_messages = final_request
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .filter(|content| content.starts_with("active run facts generation"))
        .collect::<Vec<_>>();
    assert_eq!(fact_messages, vec!["active run facts generation 2"]);
    Ok(())
}

#[tokio::test]
async fn settled_final_answer_context_removes_the_prior_snapshot() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(SideEffectTool));
    let agent = Agent::new(
        EvolvingToolFactsProvider {
            captured: Arc::clone(&captured),
            turns: AtomicUsize::new(0),
        },
        registry,
    );
    let mut session = Session::new("mock-evolving-tool-facts", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let mut delegate = SettlingToolRunFactsDelegate;

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("settle active facts"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(5),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
            &mut delegate,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    let captured = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(captured.len(), 3);
    assert!(captured[1].messages.iter().any(|message| {
        message.content.as_deref() == Some("active run still has unsettled work")
    }));
    assert!(!captured[2].messages.iter().any(|message| {
        message.content.as_deref() == Some("active run still has unsettled work")
    }));
    Ok(())
}

#[tokio::test]
async fn stable_final_answer_blocker_is_bounded_without_a_global_turn_limit() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-stable-final-blocker", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let mut delegate = SequencedFinalAnswerBlockerDelegate {
        blockers: VecDeque::new(),
        fallback: Some("stable pending child state".to_owned()),
    };

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("finish after the child"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: None,
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
            &mut delegate,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::Blocked);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswerBlocked
    );
    assert_eq!(captured.lock().expect("capture lock").len(), 4);
    Ok(())
}

#[tokio::test]
async fn evolving_final_answer_blocker_replaces_its_transient_snapshot() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-evolving-final-blocker", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let mut delegate = SequencedFinalAnswerBlockerDelegate {
        blockers: VecDeque::from([
            Some("pending child state A".to_owned()),
            Some("pending child state A".to_owned()),
            Some("unread child state B".to_owned()),
            None,
        ]),
        fallback: None,
    };

    let output = agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("finish after evolving child state"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: None,
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
            &mut delegate,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    let captured = captured.lock().expect("capture lock");
    assert_eq!(captured.len(), 4);
    let last = captured.last().expect("final request");
    assert_eq!(
        last.messages
            .iter()
            .filter(|message| { message.content.as_deref() == Some("unread child state B") })
            .count(),
        1
    );
    assert!(
        !last
            .messages
            .iter()
            .any(|message| { message.content.as_deref() == Some("pending child state A") })
    );
    Ok(())
}

#[tokio::test]
async fn agent_tool_delegate_receives_root_logical_run_identity() -> Result<()> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(AgentCategoryTool));
    let agent = Agent::new(
        ScriptedToolProvider {
            initial_chunks: vec![
                ProviderChunk::ToolCallStart {
                    id: "call-agent-logical-run".to_owned(),
                    name: "spawn_agent".to_owned(),
                },
                ProviderChunk::ToolCallArgsDelta {
                    id: "call-agent-logical-run".to_owned(),
                    delta: "{}".to_owned(),
                },
                ProviderChunk::ToolCallComplete(ToolCall {
                    id: "call-agent-logical-run".to_owned(),
                    name: "spawn_agent".to_owned(),
                    args_json: "{}".to_owned(),
                }),
                ProviderChunk::Done,
            ],
            final_text: "done".to_owned(),
        },
        registry,
    );
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;
    let mut delegate = ToolRunFactsDelegate::default();

    agent
        .run_with_approval_input_and_agent_delegate(
            &mut session,
            AgentRunInput::user("delegate").with_logical_run_id("root-logical-run-for-agent-tool"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: None,
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
            &mut delegate,
        )
        .await?;

    assert_eq!(
        delegate.root_logical_run_id.as_deref(),
        Some("root-logical-run-for-agent-tool")
    );
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
        .run_with_input(
            &mut session,
            AgentRunInput::user("hi"),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?
        .result;

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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
async fn agent_injects_durable_recorder_and_exact_route_into_tool_context() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))?;
    let saw_recorder = Arc::new(AtomicBool::new(false));
    let route_identity = Arc::new(Mutex::new(None));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RecorderAwareEchoTool {
        saw_recorder: Arc::clone(&saw_recorder),
        route_identity: Arc::clone(&route_identity),
    }));
    let agent = Agent::new(MockProvider, registry);
    let mut session = Session::new("mock", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    let result = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("hi").with_logical_run_id("tool-context-run"),
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?
        .result;

    assert_eq!(result.final_text, "done");
    assert!(saw_recorder.load(Ordering::SeqCst));
    assert_eq!(
        route_identity
            .lock()
            .expect("route identity lock should not be poisoned")
            .as_ref(),
        Some(&(
            session.session_scope_id().to_owned(),
            "tool-context-run".to_owned(),
        ))
    );
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
async fn required_agent_delegation_fails_before_provider_without_agent_tools() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = NonDelegatingTextProvider {
        calls: Arc::clone(&calls),
    };
    let agent = Agent::new(provider, ToolRegistry::new());
    let mut session = Session::new("mock", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = AutoApproveHandler;

    let error = agent
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
        )
        .await
        .expect_err("missing agent tools must fail closed");

    assert!(error.to_string().contains("no agent tools"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

#[test]
fn completed_run_terminal_records_share_one_durable_sync() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let mut session = Session::new("mock", "mock-model").with_store(store.clone());
    let before_syncs = store.writer_data_sync_count()?;
    let readiness = crate::ReadinessEvaluatedEntry {
        scope: crate::EvidenceScope::Run("message-final".to_owned()),
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

    super::run_lifecycle::append_completed_run_lifecycle_events(
        &mut session,
        AgentRunTerminalReason::FinalAnswer,
        "message-final",
        1,
        readiness,
    )?;

    assert_eq!(store.writer_data_sync_count()? - before_syncs, 1);
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[0].stored_event().event_type,
        DurableEventType::RunStatusChanged.as_str()
    );
    assert_eq!(
        records[1].stored_event().event_type,
        DurableEventType::RunFinalized.as_str()
    );
    assert_eq!(
        records[2].stored_event().event_type,
        DurableEventType::ReadinessEvaluated.as_str()
    );
    assert!(matches!(
        session.entries().last(),
        Some(SessionLogEntry::Control(ControlEntry::ReadinessEvaluated(
            _
        )))
    ));
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let expected_execution_hash = format!("sha256:{}", "2".repeat(64));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-terminal-1"
                    && execution.status == ToolExecutionStatus::Completed
                    && execution.metadata.details.get("output_hash").and_then(Value::as_str)
                        == Some(expected_execution_hash.as_str())
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TerminalTask(task))
                if task.handle.task_id.as_str() == "terminal-1"
                    && task.handle.command_sha256 == "0".repeat(64)
                    && matches!(task.status, TerminalTaskStatus::Running)
                    && task.output_preview.is_none()
                    && task.output_hash.as_deref() == Some("2".repeat(64).as_str())
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_reconciles_terminal_start_mutation_when_terminal_cancel_finishes_task() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let state = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(state.path().join("session.jsonl"))?;
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
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

#[test]
fn tool_result_bundle_is_durable_before_control_handlers_can_lock_the_session_file() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session-websearch-bundle.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let probe = Arc::new(SessionUrlRegistrarProbe::default());
    let registrar: Arc<dyn UserUrlCapabilityRegistrar> = probe.clone();
    let mut session = Session::new("mock", "mock-model").with_store(store);
    session.try_attach_user_url_capability_registrar(registrar)?;

    let urls = ["https://example.com/a", "https://example.com/b"];
    let sources = urls
        .iter()
        .enumerate()
        .map(|(index, url)| {
            ExternalSourceRecord::from_remote_candidate(
                session.session_scope_id(),
                None,
                ExternalEvidenceLevel::SearchSnippet,
                *url,
                "exa_mcp",
                Some(format!("Result {}", index + 1)),
                None,
                "2026-07-17T00:00:00Z",
                None,
                Some(index + 1),
                SourceFreshness::Unknown,
                SourceCacheStatus::NotApplicable,
                ToolRestartPolicy::Replayable,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let registrations = sources
        .iter()
        .zip(urls)
        .map(|(source, url)| UserUrlCapabilityRegistration {
            source_id: source.source_id.clone(),
            durable_entry_id: String::new(),
            raw_canonical_url: SecretString::new(url),
            safe_display_url: url.to_owned(),
            restart_policy: ToolRestartPolicy::Replayable,
            replayable_canonical_url: Some(url.to_owned()),
            originating_call_id: Some("call-websearch".to_owned()),
            provenance: WebUrlProvenanceKind::WebSearchResult,
            issued_at_ms: 1,
            expires_at_ms: 3_600_001,
        })
        .collect::<Vec<_>>();
    let result = ToolResult::ok(
        "call-websearch",
        "websearch",
        "two results",
        ToolResultMeta::default(),
    )
    .with_url_capability_registrations(registrations)
    .with_external_sources(sources);
    let mut handler = SessionReadLockingEventHandler {
        session_path: session_path.clone(),
        shared_lock: None,
        events: Vec::new(),
    };

    emit_tool_result(&mut session, &mut handler, result)?;

    assert_eq!(probe.staged.load(Ordering::SeqCst), 2);
    assert_eq!(probe.committed.load(Ordering::SeqCst), 1);
    assert_eq!(probe.rolled_back.load(Ordering::SeqCst), 0);
    assert_eq!(handler.events.len(), 4);
    assert!(matches!(
        handler.events.last(),
        Some(RunEvent::ToolResult(_))
    ));
    let locked_file = handler
        .shared_lock
        .take()
        .expect("the first URL descriptor should acquire a shared session lock");
    FileExt::unlock(&locked_file)?;
    drop(locked_file);

    let entries = JsonlSessionStore::read_entries(&session_path)?;
    assert_eq!(entries.len(), 4);
    assert!(matches!(entries[0], SessionLogEntry::ToolResultV3(_)));
    assert!(matches!(
        entries[1],
        SessionLogEntry::Control(ControlEntry::WebUrlCapabilityDescriptor(_))
    ));
    assert!(matches!(
        entries[2],
        SessionLogEntry::Control(ControlEntry::WebUrlCapabilityDescriptor(_))
    ));
    assert!(matches!(
        &entries[3],
        SessionLogEntry::Control(ControlEntry::ExternalProvenance(provenance))
            if provenance.sources.len() == 2
    ));
    Ok(())
}

#[test]
fn typed_retrieval_receipt_and_result_recover_as_one_provider_consumable_bundle() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    let mut session = Session::load_from_store("test", "model", store.clone())?;
    let artifact_ref = ToolArtifactRefV1 {
        artifact_id: "ta1_0123456789abcdef0123456789abcdef".to_owned(),
    };
    let receipt = ToolArtifactReadRecordedV1 {
        schema_version: TOOL_ARTIFACT_READ_SCHEMA_VERSION,
        call_id: "read-call-crash".to_owned(),
        artifact_ref: artifact_ref.clone(),
        source_descriptor_event_id: "source-descriptor-event".to_owned(),
        active_epoch_id: "context-epoch:test".to_owned(),
        selector: ToolArtifactSelectorV1::ByteSlice {
            offset: 0,
            limit: 32,
        },
        returned_bytes: 32,
        page_sha256: "sha256:page-digest".to_owned(),
        artifact_sha256: "sha256:artifact-digest".to_owned(),
        outcome: ToolArtifactReadOutcome::Returned,
        deduplicated_from_call_id: None,
    };
    let mut result = ToolResult::ok(
        "read-call-crash",
        "read_tool_artifact",
        json!({
            "status": "returned",
            "artifact_ref": artifact_ref,
            "returned_bytes": 32,
            "page_sha256": "sha256:page-digest",
            "artifact_sha256": "sha256:artifact-digest",
            "note": "page body is transient"
        })
        .to_string(),
        ToolResultMeta::default(),
    )
    .with_control_entry(ControlEntry::ToolArtifactRead(receipt.clone()));
    let mut handler = RecordingEventHandler::default();

    super::tool_audit::append_tool_control_entries_from_result(
        &mut session,
        &mut handler,
        &mut result,
    )?;
    assert!(matches!(
        result.control_entries.as_slice(),
        [ControlEntry::ToolArtifactRead(value)] if value == &receipt
    ));
    assert!(handler.events.is_empty());

    store.inject_writer_fault(SessionWriterFault::PartialSecondRecord)?;
    assert!(emit_tool_result(&mut session, &mut handler, result).is_err());
    assert!(handler.events.is_empty());

    // Loading invokes the writer-owned redo recovery. It completes the exact checksum-covered
    // result+receipt bytes from the fsynced bundle intent; no tool or artifact read is rerun.
    let recovered =
        Session::load_from_store("test", "model", JsonlSessionStore::new(&session_path)?)?;
    let recovered_entries = recovered.entries();
    let result_index = recovered_entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::ToolResultV3(result)
                    if result.call_id == "read-call-crash"
            )
        })
        .expect("recovered session should retain the typed retrieval result");
    assert!(matches!(
        recovered_entries.get(result_index + 1),
        Some(SessionLogEntry::Control(ControlEntry::ToolArtifactRead(value)))
            if value == &receipt
    ));
    assert!(recovered.messages().iter().any(|message| {
        message.role == MessageRole::Tool
            && message.tool_call_id.as_deref() == Some("read-call-crash")
            && message
                .content
                .as_deref()
                .is_some_and(|content| content.contains("\"status\":\"ok\""))
    }));
    let jsonl = std::fs::read_to_string(&session_path)?;
    assert!(!jsonl.contains("secret-page-body"));
    assert!(
        !session_path
            .with_extension("jsonl.append-bundle-intent")
            .exists()
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
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
    assert!(!session.entries().iter().any(|entry| match entry {
        SessionLogEntry::User(message) | SessionLogEntry::Assistant(message) => {
            message.content.as_deref() == Some("loaded transient skill body")
        }
        SessionLogEntry::ToolResultV3(result) => {
            result.initial_model_view.preview == "loaded transient skill body"
        }
        SessionLogEntry::RuntimeContextSnapshotV2(_) | SessionLogEntry::Control(_) => false,
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
                worktree_availability:
                    TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
            }),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

fn task_guidance_assessment_context() -> Result<TaskGuidanceAssessmentContext> {
    let task_id = TaskId::new("task_1")?;
    let step_id = TaskStepId::new("step_1")?;
    Ok(TaskGuidanceAssessmentContext {
        queue_id: ConversationInputQueueId::new("queue_1")?,
        task_id: task_id.clone(),
        plan_version: 2,
        dispatch_run_id: "dispatch_1".to_owned(),
        accepted_plan: TaskPlanEntry {
            task_id,
            plan_version: 2,
            status: TaskPlanStatus::Accepted,
            steps: vec![TaskStepSpec {
                step_id: step_id.clone(),
                title: "Inspect current implementation".to_owned(),
                display_name: None,
                detail: None,
                role: AgentRole::Executor,
                depends_on: Vec::new(),
                intent_refs: Vec::new(),
                mode: None,
                isolation: None,
            }],
            reason: Some("current accepted plan".to_owned()),
        },
        eligible_pending_step_ids: vec![step_id],
    })
}

#[tokio::test]
async fn task_guidance_semantics_are_selected_by_model_tool_call() -> Result<()> {
    let observed_tools = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        GuidanceDecisionProvider {
            decision: GuidanceDecision::Apply,
            observed_tools: Arc::clone(&observed_tools),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-guidance", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let exact_guidance = "请把验证顺序放到实现之前，而不是扩大任务范围";

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(exact_guidance)])
                .with_task_plan_update(TaskPlanUpdateContext {
                    task_id: TaskId::new("task_1")?,
                    max_plan_steps: 4,
                    max_plan_versions: 3,
                    worktree_availability:
                        TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
                })
                .with_task_guidance_assessment(task_guidance_assessment_context()?),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::TaskPlanAccepted);
    let observed_tools = observed_tools
        .lock()
        .expect("observed tools lock should not be poisoned");
    assert!(
        observed_tools
            .iter()
            .any(|name| name == TASK_GUIDANCE_APPLY_TOOL_NAME)
    );
    assert!(
        observed_tools
            .iter()
            .any(|name| name == TASK_PLAN_UPDATE_TOOL_NAME)
    );
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied))
                if applied.queue_id.as_str() == "queue_1"
                    && applied.plan_version == 2
                    && applied.reason == TaskGuidanceApplyReason::PrioritizesPendingStep
                    && applied.target_step_ids
                        == vec![TaskStepId::new("step_1").expect("valid step id")]
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.plan_version == 2 && plan.status == TaskPlanStatus::Accepted
        )
    }));
    let applied = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(applied)) => Some(applied),
            _ => None,
        })
        .expect("model apply decision should be durable");
    assert!(!serde_json::to_string(applied)?.contains(exact_guidance));
    Ok(())
}

#[tokio::test]
async fn task_guidance_model_can_choose_a_new_plan_version() -> Result<()> {
    let observed_tools = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        GuidanceDecisionProvider {
            decision: GuidanceDecision::Replan,
            observed_tools,
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-guidance-replan", "mock-model");
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
                "新增一个独立安全审查步骤",
            )])
            .with_task_plan_update(TaskPlanUpdateContext {
                task_id: TaskId::new("task_1")?,
                max_plan_steps: 4,
                max_plan_versions: 3,
                worktree_availability:
                    TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
            })
            .with_task_guidance_assessment(task_guidance_assessment_context()?),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::TaskPlanAccepted);
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskPlan(plan))
                if plan.plan_version == 3 && plan.status == TaskPlanStatus::Accepted
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::TaskGuidanceApplied(_))
        )
    }));
    Ok(())
}

#[tokio::test]
async fn automatic_task_routing_exposes_semantic_policy_before_the_user_turn() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(TaskHandoffSideEffectTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(
        CapturingRoutingProvider {
            captured: Arc::clone(&captured),
        },
        tools,
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let prompt = "coordinate parser and formatter changes, then run the complete verification";
    let logical_run_id = "semantic-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn: source_turn.clone(),
                    routing_policy: TaskRoutingPolicy::Auto,
                    route_capability: AutomaticRouteCapability::DirectTask,
                    writable_memory_routing: false,
                    task_continuation: None,
                    plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                    task_handoff: Some(TaskPlanningHandoffBinding {
                        handoff_id: TaskHandoffId::new("handoff-semantic-routing")?,
                        task_id: TaskId::new("task-semantic-routing")?,
                        source_turn,
                        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                        objective: prompt.to_owned(),
                        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                        requested_at_ms: 42,
                        decided_at_ms: 43,
                    }),
                },
            )));
    let mut handler = RecordingEventHandler::default();

    agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 2);
    let request = &requests[0];
    let routing_index = request
        .messages
        .iter()
        .position(|message| {
            message.content.as_deref() == Some(conversation_route_routing_contract_material())
        })
        .expect("automatic routing request should include the semantic routing policy");
    let user_index = request
        .messages
        .iter()
        .position(|message| message.content.as_deref() == Some(prompt))
        .expect("request should include the user turn");
    assert!(routing_index < user_index);
    assert_eq!(request.tools.len(), 3);
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == REQUEST_TASK_PLANNING_TOOL_NAME)
    );
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == crate::REQUEST_PLAN_REVIEW_TOOL_NAME)
    );
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME)
    );
    assert!(requests[1].tools.iter().all(|tool| {
        tool.name != REQUEST_TASK_PLANNING_TOOL_NAME
            && tool.name != CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME
    }));
    assert!(requests[1].messages.iter().any(|message| {
        message.content.as_deref()
            == Some(direct_conversation_continuation_prompt_contract_material())
    }));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(handler.events.iter().all(|event| {
        !matches!(
            event,
            RunEvent::ToolCallStarted(call)
                | RunEvent::ToolCallCompleted(call)
                if matches!(
                    call.id.as_str(),
                    "call-routing-side-effect" | "call-continue-routing"
                )
        ) && !matches!(
            event,
            RunEvent::ToolResult(result)
                if matches!(
                    result.call_id.as_str(),
                    "call-routing-side-effect" | "call-continue-routing"
                )
        ) && !matches!(
            event,
            RunEvent::Notice(message) if message.contains("routing")
        )
    }));
    assert!(
        settled_tool_results(&session)
            .iter()
            .any(|(call_id, _)| call_id == "call-routing-side-effect")
    );
    Ok(())
}

#[tokio::test]
async fn automatic_task_routing_accepts_only_an_exact_frozen_routing_candidate() -> Result<()> {
    let root = tempfile::tempdir()?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingRoutingProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing-routing", "mock-model");
    let safe_prompt = "inspect the queued request with authorization=[redacted]";
    let exact_prompt = "inspect the queued request with authorization=super-secret-value";
    let logical_run_id = "frozen-routing-run";
    session.append_user_message(ModelMessage::user("unrelated earlier request"))?;
    session.append_assistant_message(ModelMessage::assistant(
        Some("queued request context evidence".to_owned()),
        Vec::new(),
    ))?;
    let mut durable_user = ModelMessage::user(safe_prompt);
    durable_user.id = "frozen-routing-user".to_owned();
    let options = AgentRunOptions {
        workspace_root: root.path().to_path_buf(),
        max_turns: Some(2),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
    };
    let mut exact_user = ModelMessage::user(exact_prompt);
    exact_user.id = durable_user.id.clone();
    let request = session.build_pre_turn_candidate_request(
        root.path(),
        &options.memory_config,
        route_surface_tool_specs(AutomaticRouteCapability::DirectTask),
        None,
        options.reasoning_effort.clone(),
        None,
        None,
        &[
            ModelMessage::system(conversation_route_routing_contract_material()),
            exact_user,
        ],
        RuntimeContextCandidates::default(),
        &[],
    )?;
    let frozen =
        FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request.clone())?;
    session.append_user_message(durable_user.clone())?;
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        durable_user.id.clone(),
        logical_run_id,
    )?;
    let input = AgentRunInput::without_persisted_user_message(Vec::new())
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn: source_turn.clone(),
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::DirectTask,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: Some(test_plan_review_handoff_binding(&source_turn, safe_prompt)),
                task_handoff: Some(TaskPlanningHandoffBinding {
                    handoff_id: TaskHandoffId::new("handoff-frozen-routing")?,
                    task_id: TaskId::new("task-frozen-routing")?,
                    source_turn,
                    parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                    objective: safe_prompt.to_owned(),
                    policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                    route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                    requested_at_ms: 42,
                    decided_at_ms: 43,
                }),
            },
        )))
        .with_initial_frozen_provider_request(frozen);
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(&mut session, input, options, &mut handler)
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0]
            .messages
            .iter()
            .find(|message| message.id == durable_user.id)
            .and_then(|message| message.content.as_deref()),
        Some(exact_prompt)
    );
    assert_eq!(
        requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        vec![
            crate::REQUEST_PLAN_REVIEW_TOOL_NAME,
            REQUEST_TASK_PLANNING_TOOL_NAME,
            CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME
        ]
    );
    Ok(())
}

#[tokio::test]
async fn automatic_task_routing_rejects_a_frozen_ordinary_tool_request() -> Result<()> {
    let root = tempfile::tempdir()?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingRoutingProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing-routing", "mock-model");
    let prompt = "inspect the queued request";
    let logical_run_id = "invalid-frozen-routing-run";
    let mut durable_user = ModelMessage::user(prompt);
    durable_user.id = "invalid-frozen-routing-user".to_owned();
    session.append_user_message(durable_user.clone())?;
    let request = session.build_pre_turn_candidate_request(
        root.path(),
        &MemoryConfig::with_enabled(false),
        Vec::new(),
        None,
        Some(ReasoningEffort::Medium),
        None,
        None,
        &[],
        RuntimeContextCandidates::default(),
        &[],
    )?;
    let frozen = FrozenProviderRequestMaterial::freeze(session.session_scope_id(), request)?;
    let source_turn =
        ConversationTurnRef::new(session.session_scope_id(), durable_user.id, logical_run_id)?;
    let input = AgentRunInput::without_persisted_user_message(Vec::new())
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn: source_turn.clone(),
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::DirectTask,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                task_handoff: Some(TaskPlanningHandoffBinding {
                    handoff_id: TaskHandoffId::new("handoff-invalid-frozen-routing")?,
                    task_id: TaskId::new("task-invalid-frozen-routing")?,
                    source_turn,
                    parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                    objective: prompt.to_owned(),
                    policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                    route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                    requested_at_ms: 42,
                    decided_at_ms: 43,
                }),
            },
        )))
        .with_initial_frozen_provider_request(frozen);
    let mut handler = crate::event::NoopEventHandler;

    let error = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: root.path().to_path_buf(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("ordinary frozen request must not bypass routing-only materialization");

    assert!(error.to_string().contains("automatic routing"));
    assert!(
        captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn task_participant_system_contract_precedes_the_step_prompt() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let step_prompt = "Edit src/lib.rs for the accepted parser step.";
    let input =
        AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(step_prompt)])
            .with_run_purpose(AgentRunPurpose::TaskParticipant(TaskParticipantContext {
                task_id: TaskId::new("task-participant-contract")?,
                plan_version: 1,
                step_id: TaskStepId::new("parser-step")?,
                attempt_id: TaskParticipantAttemptId::new("participant-attempt-1")?,
            }));
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    let request = requests.first().expect("participant request");
    let contract_index = request
        .messages
        .iter()
        .position(|message| {
            message.content.as_deref() == Some(task_participant_system_prompt_contract_material())
        })
        .expect("participant request should include its system contract");
    let prompt_index = request
        .messages
        .iter()
        .position(|message| message.content.as_deref() == Some(step_prompt))
        .expect("participant request should include the step prompt");
    assert!(contract_index < prompt_index);
    Ok(())
}

#[tokio::test]
async fn task_participant_forces_toolless_finalization_after_post_mutation_read_tail() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WorkspaceMutatingCustomTool));
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        PostMutationReadLoopProvider {
            calls: Arc::clone(&calls),
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session = Session::new("mock-post-mutation-read-loop", "mock-model").with_store(store);
    let input = AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
        "Apply the accepted mutation and return the result.",
    )])
    .with_run_purpose(AgentRunPurpose::TaskParticipant(TaskParticipantContext {
        task_id: TaskId::new("task-convergence")?,
        plan_version: 1,
        step_id: TaskStepId::new("write-step")?,
        attempt_id: TaskParticipantAttemptId::new("participant-convergence-1")?,
    }));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(20),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(
        output.result.final_text,
        "mutation complete after bounded inspection"
    );
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    assert_eq!(
        output.result.tool_calls,
        crate::TASK_STEP_NO_PROGRESS_FINALIZE_THRESHOLD as usize + 2
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        crate::TASK_STEP_NO_PROGRESS_FINALIZE_THRESHOLD as usize + 3
    );
    assert!(
        output.result.tool_calls < TASK_PARTICIPANT_POST_MUTATION_READ_TAIL_LIMIT + 1,
        "semantic no-progress finalization should preempt the coarser post-mutation read tail"
    );
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    let final_request = requests.last().expect("finalization request");
    assert!(final_request.tools.is_empty());
    assert!(final_request.hosted_tools.is_empty());
    assert!(final_request.messages.iter().any(|message| {
        message.content.as_deref() == Some(task_participant_finalization_prompt_contract_material())
    }));
    assert!(
        requests[..requests.len() - 1]
            .iter()
            .all(|request| !request.tools.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn task_participant_forces_toolless_finalization_after_repeated_semantic_frontier()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        RepeatedReadLoopProvider {
            calls: Arc::clone(&calls),
            captured: Arc::clone(&captured),
            finalization_text: None,
        },
        registry,
    );
    let mut session = Session::new("mock-repeated-read-loop", "mock-model").with_store(store);
    let attempt_id = TaskParticipantAttemptId::new("participant-repeated-frontier-1")?;
    let input = AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
        "Inspect the accepted step and return a bounded result.",
    )])
    .with_run_purpose(AgentRunPurpose::TaskParticipant(TaskParticipantContext {
        task_id: TaskId::new("task-repeated-frontier")?,
        plan_version: 1,
        step_id: TaskStepId::new("read-step")?,
        attempt_id: attempt_id.clone(),
    }));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(20),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(
        output.result.final_text,
        "bounded result after repeated analysis"
    );
    assert_eq!(output.result.tool_calls, 3);
    assert_eq!(calls.load(Ordering::SeqCst), 4);
    let checkpoints = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::TaskStepCheckpointV2(checkpoint))
                if checkpoint.attempt_id == attempt_id =>
            {
                Some(checkpoint.no_progress_count)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(checkpoints, vec![0, 1, 2]);
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    let final_request = requests.last().expect("finalization request");
    assert!(final_request.tools.is_empty());
    assert!(final_request.messages.iter().any(|message| {
        message.content.as_deref() == Some(task_participant_finalization_prompt_contract_material())
    }));
    Ok(())
}

#[test]
fn task_checkpoint_treats_artifact_pagination_and_changed_output_as_progress() -> Result<()> {
    let context = TaskParticipantContext {
        task_id: TaskId::new("task-artifact-pagination")?,
        plan_version: 1,
        step_id: TaskStepId::new("read-artifact")?,
        attempt_id: TaskParticipantAttemptId::new("participant-artifact-pagination-1")?,
    };
    let artifact_ref = ToolArtifactRefV1 {
        artifact_id: "ta1_0123456789abcdef0123456789abcdef".to_owned(),
    };
    artifact_ref.validate()?;
    let first_call = ToolCall {
        id: "page-1".to_owned(),
        name: "read_tool_artifact".to_owned(),
        args_json: json!({
            "artifact_ref": artifact_ref,
            "selector": {"kind": "line_page", "start_line": 1, "max_lines": 200}
        })
        .to_string(),
    };
    let second_call = ToolCall {
        id: "page-2".to_owned(),
        name: "read_tool_artifact".to_owned(),
        args_json: json!({
            "artifact_ref": ToolArtifactRefV1 {
                artifact_id: "ta1_0123456789abcdef0123456789abcdef".to_owned(),
            },
            "selector": {"kind": "line_page", "start_line": 201, "max_lines": 200}
        })
        .to_string(),
    };
    let first_result = ToolResult::ok(
        "page-1",
        "read_tool_artifact",
        "lines 1 through 200",
        ToolResultMeta {
            returned_lines: Some(200),
            total_lines: Some(400),
            truncated: true,
            ..ToolResultMeta::default()
        },
    );
    let second_result = ToolResult::ok(
        "page-2",
        "read_tool_artifact",
        "lines 201 through 400",
        ToolResultMeta {
            returned_lines: Some(200),
            total_lines: Some(400),
            ..ToolResultMeta::default()
        },
    );

    let first = build_task_step_checkpoint(&context, 1, &[(first_call, first_result)], &[], None)?;
    let second = build_task_step_checkpoint(
        &context,
        2,
        &[(second_call, second_result)],
        &[],
        Some(&first),
    )?;

    assert_ne!(first.semantic_call_hash, second.semantic_call_hash);
    assert_ne!(first.result_frontier_hash, second.result_frontier_hash);
    assert_eq!(second.no_progress_count, 0);
    Ok(())
}

#[test]
fn task_checkpoint_detects_repeated_bounded_observation_only_when_output_is_unchanged() -> Result<()>
{
    let context = TaskParticipantContext {
        task_id: TaskId::new("task-bounded-observation")?,
        plan_version: 1,
        step_id: TaskStepId::new("inspect")?,
        attempt_id: TaskParticipantAttemptId::new("participant-bounded-observation-1")?,
    };
    let checkpoint = |turn, command: &str, output: &str, previous| {
        let result = ToolResult::ok(
            format!("call-{turn}"),
            "bash",
            output,
            ToolResultMeta::default(),
        );
        build_task_step_checkpoint(
            &context,
            turn,
            &[(
                &ToolCall {
                    id: format!("call-{turn}"),
                    name: "bash".to_owned(),
                    args_json: json!({"command": command}).to_string(),
                },
                &result,
            )]
            .into_iter()
            .map(|(call, result)| (call.clone(), result.clone()))
            .collect::<Vec<_>>(),
            &[],
            previous,
        )
    };

    let first = checkpoint(1, "git status", "clean", None)?;
    let cosmetic_rewrite = checkpoint(2, "git status --short", "clean", Some(&first))?;
    assert_eq!(cosmetic_rewrite.no_progress_count, 1);
    let changed_output = checkpoint(
        3,
        "git status --short",
        "M crates/sigil-kernel/src/agent.rs",
        Some(&cosmetic_rewrite),
    )?;
    assert_eq!(changed_output.no_progress_count, 0);
    assert_ne!(
        cosmetic_rewrite.result_frontier_hash,
        changed_output.result_frontier_hash
    );
    Ok(())
}

#[tokio::test]
async fn task_participant_enters_repair_replan_after_empty_finalization() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        RepeatedReadLoopProvider {
            calls: Arc::clone(&calls),
            captured: Arc::clone(&captured),
            finalization_text: Some(String::new()),
        },
        registry,
    );
    let mut session = Session::new("mock-empty-finalization", "mock-model").with_store(store);
    let input = AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
        "Inspect the accepted step and return a bounded result.",
    )])
    .with_run_purpose(AgentRunPurpose::TaskParticipant(TaskParticipantContext {
        task_id: TaskId::new("task-empty-finalization")?,
        plan_version: 1,
        step_id: TaskStepId::new("read-step")?,
        attempt_id: TaskParticipantAttemptId::new("participant-empty-finalization-1")?,
    }));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: temp.path().to_path_buf(),
                max_turns: Some(20),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "");
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::RepairReplanRequired
    );
    assert_eq!(output.disposition, AgentRunDisposition::Blocked);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        crate::TASK_STEP_NO_PROGRESS_FINALIZE_THRESHOLD as usize + 2
    );
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert!(
        requests
            .last()
            .is_some_and(|request| request.tools.is_empty())
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_conversation_does_not_inherit_task_participant_convergence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let workspace = temp.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let store = JsonlSessionStore::new(temp.path().join("state/session.jsonl"))?;
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WorkspaceMutatingCustomTool));
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        PostMutationReadLoopProvider {
            calls: Arc::clone(&calls),
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session = Session::new("mock-post-mutation-read-loop", "mock-model").with_store(store);
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("Keep inspecting."),
            AgentRunOptions {
                workspace_root: workspace,
                max_turns: Some(8),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::MaxTurns
    );
    assert_eq!(output.disposition, AgentRunDisposition::Interrupted);
    assert_eq!(calls.load(Ordering::SeqCst), 8);
    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert!(requests.iter().all(|request| !request.tools.is_empty()));
    assert!(requests.iter().all(|request| {
        request.messages.iter().all(|message| {
            message.content.as_deref()
                != Some(task_participant_finalization_prompt_contract_material())
        })
    }));
    Ok(())
}

#[tokio::test]
async fn automatic_task_routing_degrades_to_ordinary_conversation_after_two_untyped_decisions()
-> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(TaskHandoffSideEffectTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(
        DegradingRoutingProvider {
            calls: Arc::clone(&calls),
        },
        tools,
    );
    let mut session = Session::new("mock-untyped-routing", "mock-model");
    let prompt = "coordinate parser and formatter changes";
    let logical_run_id = "untyped-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn: source_turn.clone(),
                    routing_policy: TaskRoutingPolicy::Auto,
                    route_capability: AutomaticRouteCapability::DirectTask,
                    writable_memory_routing: false,
                    task_continuation: None,
                    plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                    task_handoff: Some(TaskPlanningHandoffBinding {
                        handoff_id: TaskHandoffId::new("handoff-untyped-routing")?,
                        task_id: TaskId::new("task-untyped-routing")?,
                        source_turn,
                        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                        objective: prompt.to_owned(),
                        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                        requested_at_ms: 42,
                        decided_at_ms: 43,
                    }),
                },
            )));
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    assert_eq!(output.result.final_text, "final answer");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "two routing microturns then one degraded ordinary turn"
    );
    assert!(session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant && message.content.as_deref() == Some("final answer")
    }));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(handler.events.iter().all(|event| {
        !matches!(
            event,
            RunEvent::ToolCallStarted(call)
                | RunEvent::ToolCallCompleted(call)
                if call.id == "call-invalid-routing-tool"
        ) && !matches!(
            event,
            RunEvent::ToolResult(result) if result.call_id == "call-invalid-routing-tool"
        ) && !matches!(
            event,
            RunEvent::Notice(message) if message.contains("routing")
        )
    }));
    assert!(
        settled_tool_results(&session)
            .iter()
            .any(|(call_id, preview)| {
                call_id == "call-invalid-routing-tool"
                    && preview.contains(
                        "ordinary tools are not available during the typed task-routing microturn",
                    )
            })
    );
    assert!(
        session.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(_))
        )),
        "the degraded conversation must record the chat route decision"
    );
    Ok(())
}

#[tokio::test]
async fn automatic_task_routing_degrades_a_pure_free_text_microturn_to_ordinary_conversation()
-> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-pure-free-text-routing", "mock-model");
    let prompt = "hello";
    let logical_run_id = "pure-free-text-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn: source_turn.clone(),
                    routing_policy: TaskRoutingPolicy::Auto,
                    route_capability: AutomaticRouteCapability::DirectTask,
                    writable_memory_routing: false,
                    task_continuation: None,
                    plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                    task_handoff: Some(TaskPlanningHandoffBinding {
                        handoff_id: TaskHandoffId::new("handoff-pure-free-text")?,
                        task_id: TaskId::new("task-pure-free-text")?,
                        source_turn,
                        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                        objective: prompt.to_owned(),
                        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                        requested_at_ms: 42,
                        decided_at_ms: 43,
                    }),
                },
            )));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::FinalAnswer
    );
    assert_eq!(output.result.final_text, "captured");
    assert_eq!(
        captured
            .lock()
            .expect("captured requests lock should not be poisoned")
            .len(),
        3,
        "two routing microturns then one degraded ordinary turn"
    );
    assert!(session.messages().iter().any(|message| {
        message.role == MessageRole::Assistant && message.content.as_deref() == Some("captured")
    }));
    assert!(
        session.entries().iter().any(|entry| matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(_))
        )),
        "the degraded conversation must record the chat route decision"
    );
    Ok(())
}

struct QueuedFollowUpTextProvider {
    calls: Arc<AtomicUsize>,
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

#[async_trait]
impl Provider for QueuedFollowUpTextProvider {
    fn name(&self) -> &str {
        "mock-queued-follow-up"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        CapturingTextProvider {
            captured: Arc::new(Mutex::new(Vec::new())),
        }
        .capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured
            .lock()
            .expect("captured request lock should not be poisoned")
            .push(request);
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        let text = if turn == 0 {
            "first answer"
        } else {
            "answer to the follow-up"
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(text.to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

struct OneShotPendingInputProvider {
    remaining: AtomicUsize,
}

#[async_trait]
impl PendingConversationInputProvider for OneShotPendingInputProvider {
    async fn promote_next_pending_input(
        &self,
        session: &mut Session,
        _logical_run_id: &str,
    ) -> Result<Option<PromotedConversationInput>> {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) != 1 {
            return Ok(None);
        }
        session.append_user_message(ModelMessage::user("queued follow-up"))?;
        let body = "context resolved for the queued follow-up";
        let mut runtime_context = RuntimeContextCandidates::new();
        runtime_context.items.push(ContextItem {
            id: "queued-follow-up-context".to_owned(),
            source: ContextSource::RepositoryFile,
            source_event_id: None,
            trust_level: ContextTrustLevel::UntrustedRepositoryData,
            sensitivity: ContextSensitivity::Repository,
            egress_decision: None,
            repo_revision: Some("queued-follow-up-snapshot".to_owned()),
            token_cost: crate::estimate_context_token_cost(body),
            score: Some(100.0),
            score_breakdown: Vec::new(),
            inclusion_reason: ContextInclusionReason::RetrievalHit,
            body_ref: ContextBodyRef::inline(body),
        });
        runtime_context
            .snippets
            .insert("queued-follow-up-context".to_owned(), body.to_owned());
        Ok(Some(PromotedConversationInput {
            prompt: "queued follow-up".to_owned(),
            runtime_context,
        }))
    }
}

#[tokio::test]
async fn queued_follow_up_is_injected_at_the_final_answer_gate_without_interrupting() -> Result<()>
{
    let calls = Arc::new(AtomicUsize::new(0));
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        QueuedFollowUpTextProvider {
            calls: Arc::clone(&calls),
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-follow-up-injection", "mock-model");
    let prompt = "original question";
    let logical_run_id = "follow-up-injection-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_pending_input_provider(Arc::new(OneShotPendingInputProvider {
            remaining: AtomicUsize::new(1),
        }))
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn,
                routing_policy: TaskRoutingPolicy::Manual,
                route_capability: AutomaticRouteCapability::Unsupported,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: None,
                task_handoff: None,
            },
        )));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_context: crate::PermissionEvaluationContext::default(),
                permission_mode_override: None,
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(output.result.final_text, "answer to the follow-up");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let captured = captured
        .lock()
        .expect("captured request lock should not be poisoned");
    assert_eq!(captured.len(), 2);
    assert!(
        !format!("{:?}", captured[0].messages)
            .contains("context resolved for the queued follow-up")
    );
    assert!(
        format!("{:?}", captured[1].messages).contains("context resolved for the queued follow-up")
    );
    let messages = session.messages();
    assert!(messages.iter().any(|message| {
        message.role == MessageRole::User && message.content.as_deref() == Some("queued follow-up")
    }));
    assert!(messages.iter().any(|message| {
        message.role == MessageRole::Assistant && message.content.as_deref() == Some("first answer")
    }));
    assert!(messages.iter().any(|message| {
        message.role == MessageRole::Assistant
            && message.content.as_deref() == Some("answer to the follow-up")
    }));
    Ok(())
}

#[tokio::test]
async fn automatic_task_routing_rejects_a_handoff_after_the_negative_decision() -> Result<()> {
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        LateTaskHandoffProvider {
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-late-task-handoff", "mock-model");
    let prompt = "what does this symbol mean?";
    let logical_run_id = "late-handoff-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn: source_turn.clone(),
                    routing_policy: TaskRoutingPolicy::Auto,
                    route_capability: AutomaticRouteCapability::DirectTask,
                    writable_memory_routing: false,
                    task_continuation: None,
                    plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                    task_handoff: Some(TaskPlanningHandoffBinding {
                        handoff_id: TaskHandoffId::new("handoff-late-routing")?,
                        task_id: TaskId::new("task-late-routing")?,
                        source_turn,
                        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                        objective: prompt.to_owned(),
                        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                        requested_at_ms: 42,
                        decided_at_ms: 43,
                    }),
                },
            )));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(output.result.final_text, "ordinary answer");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(
                ControlEntry::TaskHandoffRequested(_)
                    | ControlEntry::TaskHandoffResolved(_)
                    | ControlEntry::TaskRun(_)
            )
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::ToolResultV3(result)
                if result.call_id == "call-late-handoff"
                    && result
                        .initial_model_view
                        .preview
                        .contains("not available after the routing microturn")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn manual_task_routing_exposes_neither_automatic_policy_nor_tool() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-capturing", "mock-model");
    let prompt = "rename one local helper";
    let logical_run_id = "manual-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn,
                    routing_policy: TaskRoutingPolicy::Manual,
                    route_capability: AutomaticRouteCapability::Unsupported,
                    writable_memory_routing: false,
                    task_continuation: None,
                    plan_review: None,
                    task_handoff: None,
                },
            )));
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(2),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let requests = captured
        .lock()
        .expect("captured requests lock should not be poisoned");
    assert_eq!(requests.len(), 1);
    assert!(requests[0].messages.iter().all(|message| {
        message.content.as_deref() != Some(conversation_route_routing_contract_material())
    }));
    assert!(
        requests[0]
            .tools
            .iter()
            .all(|tool| tool.name != REQUEST_TASK_PLANNING_TOOL_NAME)
    );
    Ok(())
}

#[tokio::test]
async fn accepted_task_handoff_is_typed_durable_and_ignores_the_rest_of_the_batch() -> Result<()> {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskHandoffSideEffectTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(TaskHandoffProvider, registry);
    let mut session = Session::new("mock-task-handoff", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let prompt = "ship the cross-crate orchestration change";
    let logical_run_id = "foreground-run-1";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let handoff_id = TaskHandoffId::new("handoff-automatic-1")?;
    let task_id = TaskId::new("task-automatic-1")?;
    let cancellation_owner = RunCancellationOwner::new();
    let cancellation_scope_id = cancellation_owner.handle().scope_id().to_owned();
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn: source_turn.clone(),
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::DirectTask,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                task_handoff: Some(TaskPlanningHandoffBinding {
                    handoff_id: handoff_id.clone(),
                    task_id: task_id.clone(),
                    source_turn: source_turn.clone(),
                    parent_session_ref: SessionRef::new_relative("session.jsonl")?,
                    objective: prompt.to_owned(),
                    policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                    route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
                    requested_at_ms: 42,
                    decided_at_ms: 43,
                }),
            },
        )))
        .with_cancellation(cancellation_owner.handle());

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let AgentRunDisposition::StartDurableTask(action) = &output.disposition else {
        panic!("accepted handoff must return a typed start action");
    };
    assert_eq!(action.handoff_id, handoff_id);
    assert_eq!(action.task_id, task_id);
    assert_eq!(action.source_turn, source_turn);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::TaskHandoff
    );
    assert!(output.result.final_text.is_empty());
    assert!(output.result.final_message_id.is_none());
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let scope_binding_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRunCancellationScopeBound(binding))
                    if binding.task_id == task_id && binding.run_scope_id == cancellation_scope_id
            )
        })
        .expect("task handoff must bind the root cancellation scope");
    let request_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            )
        })
        .expect("task handoff request must be durable");
    let task_started_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskRun(run))
                    if run.task_id == task_id && run.status == TaskRunStatus::Started
            )
        })
        .expect("task run must be durable");
    assert!(scope_binding_index < request_index);
    assert!(request_index < task_started_index);
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
            ))
            .count(),
        1
    );
    assert_eq!(
        session
            .entries()
            .iter()
            .filter(|entry| matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::TaskHandoffResolved(_))
            ))
            .count(),
        1
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskRun(run))
            if run.task_id == action.task_id && run.status == TaskRunStatus::Started
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-side-effect"
                && execution.status == ToolExecutionStatus::Cancelled
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-handoff-2"
                && execution.status == ToolExecutionStatus::Cancelled
    )));
    assert!(!session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Assistant(message)
            if message.assistant_kind == Some(AssistantMessageKind::FinalAnswer)
    )));
    Ok(())
}

#[tokio::test]
async fn accepted_task_continuation_is_typed_and_ignores_ordinary_tools() -> Result<()> {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(TaskHandoffSideEffectTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(TaskContinuationProvider, registry);
    let mut session = Session::new("mock-task-continuation", "mock-model");
    let task_id = TaskId::new("task-current-1")?;
    let parent_session_ref = SessionRef::new_relative("session.jsonl")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: "ship the original task".to_owned(),
        title: None,
        status: TaskRunStatus::Started,
        reason: None,
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step-current-1")?,
            title: "implement original scope".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: Some("accepted v1".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: parent_session_ref.clone(),
        objective: "ship the original task".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: None,
    }))?;

    let prompt = "continue, but also add the compatibility check";
    let logical_run_id = "task-continuation-run-1";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let prompt_projection = crate::project_conversation_prompt_for_persistence(prompt);
    let route_contract_fingerprint = "sha256:task-continuation-contract-v1".to_owned();
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn: source_turn.clone(),
                    routing_policy: TaskRoutingPolicy::Auto,
                    route_capability: AutomaticRouteCapability::DirectTask,
                    writable_memory_routing: false,
                    task_continuation: Some(TaskContinuationHandoffBinding {
                        task_id: task_id.clone(),
                        source_turn: source_turn.clone(),
                        plan_version: Some(1),
                        task_status: TaskRunStatus::Paused,
                        plan_status: Some(TaskPlanStatus::Accepted),
                        effective_capability: AutomaticRouteCapability::DirectTask,
                        policy_snapshot_hash: "sha256:task-continuation-policy-v1".to_owned(),
                        route_contract_fingerprint: route_contract_fingerprint.clone(),
                        decided_at_ms: 43,
                        exact_guidance: SecretString::new(prompt),
                        prompt_hash: prompt_projection.prompt_hash,
                        exact_prompt_required: prompt_projection.exact_prompt_required,
                        safe_guidance: prompt_projection.safe_prompt,
                    }),
                    plan_review: Some(test_plan_review_handoff_binding(&source_turn, prompt)),
                    task_handoff: Some(TaskPlanningHandoffBinding {
                        handoff_id: TaskHandoffId::new("handoff-decoy-for-continuation")?,
                        task_id: TaskId::new("task-decoy-for-continuation")?,
                        source_turn: source_turn.clone(),
                        parent_session_ref,
                        objective: prompt.to_owned(),
                        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
                        route_contract_fingerprint,
                        requested_at_ms: 42,
                        decided_at_ms: 43,
                    }),
                },
            )));
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let AgentRunDisposition::ContinueDurableTask(action) = &output.disposition else {
        panic!("accepted continuation must return a typed continuation action")
    };
    assert_eq!(action.task_id, task_id);
    assert_eq!(action.source_turn, source_turn);
    assert_eq!(action.plan_version, Some(1));
    assert_eq!(action.task_status, TaskRunStatus::Paused);
    assert_eq!(action.guidance.expose_secret(), prompt);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::TaskHandoff
    );
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
            if selected == &action.guidance_receipt
    )));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-side-effect-before-continuation"
                && execution.status == ToolExecutionStatus::Cancelled
    )));
    Ok(())
}

#[test]
fn exact_natural_language_reentry_reuses_pending_selection_without_forking() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let session_path = temp.path().join("pending-exact-natural-reentry.jsonl");
    let store = JsonlSessionStore::new(&session_path)?;
    crate::session::append_current_test_session_identity(&store)?;
    let mut session = Session::new("mock-task-continuation", "mock-model").with_store(store);
    let task_id = TaskId::new("task-pending-exact-selection")?;
    session.append_control(ControlEntry::TaskRun(TaskRunEntry {
        task_id: task_id.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: "finish the pending exact continuation".to_owned(),
        title: None,
        status: TaskRunStatus::Paused,
        reason: Some("crashed after typed selection".to_owned()),
    }))?;
    session.append_control(ControlEntry::TaskPlan(TaskPlanEntry {
        task_id: task_id.clone(),
        plan_version: 1,
        status: TaskPlanStatus::Accepted,
        steps: vec![TaskStepSpec {
            step_id: TaskStepId::new("step-pending-exact")?,
            title: "apply exact recovery guidance".to_owned(),
            display_name: None,
            detail: None,
            role: AgentRole::Executor,
            depends_on: Vec::new(),
            intent_refs: Vec::new(),
            mode: None,
            isolation: None,
        }],
        reason: None,
    }))?;

    let exact_guidance = "finish step with authorization=original-secret";
    let projected = crate::project_conversation_prompt_for_persistence(exact_guidance);
    assert!(projected.exact_prompt_required);
    let old_source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        "message-pending-exact-selection",
        "run-pending-exact-selection",
    )?;
    let mut old_source = ModelMessage::user(projected.safe_prompt.clone());
    old_source.id = old_source_turn.message_id.clone();
    session.append_user_message(old_source)?;
    let old_route_fingerprint = "sha256:pending-exact-selection-route".to_owned();
    session.append_controls(vec![
        ControlEntry::ConversationRouteDecisionRecorded(
            crate::ConversationRouteDecisionRecordedEntry {
                decision_id: conversation_route_decision_id_for_source(&old_source_turn),
                source_turn: old_source_turn.clone(),
                route: ConversationRoute::Task,
                reason_codes: Vec::new(),
                configured_policy: TaskRoutingPolicy::Auto,
                effective_capability: AutomaticRouteCapability::DirectTask,
                policy_snapshot_hash: "sha256:pending-exact-selection-policy".to_owned(),
                route_contract_fingerprint: old_route_fingerprint.clone(),
                decided_at_ms: 1,
            },
        ),
        ControlEntry::TaskContinuationSelected(crate::TaskContinuationSelectedEntry {
            task_id: task_id.clone(),
            source_turn: old_source_turn.clone(),
            plan_version: Some(1),
            task_status: TaskRunStatus::Paused,
            plan_status: Some(TaskPlanStatus::Accepted),
            route_contract_fingerprint: old_route_fingerprint.clone(),
            control: crate::TaskContinuationControlKind::ApplyCurrentRequestAsGuidance,
            prompt_hash: projected.prompt_hash.clone(),
            exact_prompt_required: projected.exact_prompt_required,
            guidance: projected.safe_prompt.clone(),
            selected_at_ms: 1,
        }),
    ])?;

    {
        let mut invoke = |message_id: &str,
                          logical_run_id: &str,
                          guidance: &str,
                          call_id: &str|
         -> Result<(Option<crate::ContinueDurableTaskAction>, usize)> {
            let source_turn =
                ConversationTurnRef::new(session.session_scope_id(), message_id, logical_run_id)?;
            let source_projection = crate::project_conversation_prompt_for_persistence(guidance);
            let mut source = ModelMessage::user(source_projection.safe_prompt.clone());
            source.id = source_turn.message_id.clone();
            session.append_user_message(source)?;
            let binding = TaskContinuationHandoffBinding {
                task_id: task_id.clone(),
                source_turn,
                plan_version: Some(1),
                task_status: TaskRunStatus::Paused,
                plan_status: Some(TaskPlanStatus::Accepted),
                effective_capability: AutomaticRouteCapability::DirectTask,
                policy_snapshot_hash: "sha256:new-exact-reentry-policy".to_owned(),
                route_contract_fingerprint: format!("sha256:{logical_run_id}"),
                decided_at_ms: 2,
                exact_guidance: SecretString::new(guidance),
                prompt_hash: source_projection.prompt_hash,
                exact_prompt_required: source_projection.exact_prompt_required,
                safe_guidance: source_projection.safe_prompt,
            };
            let call = ToolCall {
                id: call_id.to_owned(),
                name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
                args_json: r#"{"reason":"continue_current_task","action":"apply_current_request_as_guidance"}"#.to_owned(),
            };
            let mut handler = RecordingEventHandler::default();
            let mut outcome = AgentRunOutcome::default();
            let mut batch_results = Vec::new();
            let action = super::handle_continue_existing_task_call(
                &mut session,
                &mut handler,
                &mut outcome,
                &call,
                &binding,
                Some("scope-pending-exact-reentry"),
                &mut batch_results,
            )?;
            let selection_count = session
                .entries()
                .iter()
                .filter(|entry| {
                    matches!(
                        entry,
                        SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
                            if selected.task_id == task_id
                    )
                })
                .count();
            Ok((action, selection_count))
        };

        let (mismatched, mismatched_selection_count) = invoke(
            "message-pending-exact-mismatch",
            "run-pending-exact-mismatch",
            "replace the plan with authorization=different-secret",
            "call-pending-exact-mismatch",
        )?;
        assert!(mismatched.is_none());
        assert_eq!(
            mismatched_selection_count, 1,
            "mismatched natural-language re-entry must not append another selection"
        );

        let (action, recovered_selection_count) = invoke(
            "message-pending-exact-reentry",
            "run-pending-exact-reentry",
            exact_guidance,
            "call-pending-exact-reentry",
        )?;
        let action =
            action.expect("matching natural-language re-entry must reuse the pending selection");
        assert_eq!(action.source_turn, old_source_turn);
        assert_eq!(action.route_contract_fingerprint, old_route_fingerprint);
        assert_eq!(action.guidance.expose_secret(), exact_guidance);
        assert_eq!(
            recovered_selection_count, 1,
            "matching natural-language re-entry must reuse, not duplicate, the old receipt"
        );
    }
    drop(session);

    let reopened = Session::load_from_store(
        "mock-task-continuation",
        "mock-model",
        JsonlSessionStore::new(&session_path)?,
    )?;
    assert_eq!(
        reopened
            .task_state_projection()
            .current_task()
            .map(|task| &task.task_id),
        Some(&task_id),
        "a crash after handler return must retain the exact Task as the current recovery target"
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::TaskContinuationSelected(selected))
                        if selected.task_id == task_id
                )
            })
            .count(),
        1
    );
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    SessionLogEntry::Control(ControlEntry::TaskRunTargetSelected(selected))
                        if selected.task_id == task_id
                            && selected.run_scope_id == "scope-pending-exact-reentry"
                )
            })
            .count(),
        1
    );
    let recovered =
        crate::recoverable_task_guidance_review(&reopened, &task_id, Some(exact_guidance))?.expect(
            "the original pending authority remains recoverable after handler-boundary restart",
        );
    assert!(matches!(
        recovered.authority,
        crate::RecoverableTaskGuidanceReviewAuthority::ContinuationSelected(selected)
            if selected.source_turn == old_source_turn
    ));
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
                worktree_availability:
                    TaskPlannerWorktreeAvailability::AvailableWithInteractiveReview,
            }),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
#[derive(Clone, Copy)]
enum GuidanceDecision {
    Apply,
    Replan,
}
struct GuidanceDecisionProvider {
    decision: GuidanceDecision,
    observed_tools: Arc<Mutex<Vec<String>>>,
}

#[tokio::test]
async fn automatic_routing_plan_review_decision_records_route_and_starts_review() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        PlanReviewRoutingProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan-review-routing", "mock-model");
    let prompt = "propose how to restructure the coordinator before implementing";
    let logical_run_id = "plan-review-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let plan_review_binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let expected_review_id = plan_review_binding.plan_review_id.clone();
    let expected_message_id = plan_review_binding.source_turn.message_id.clone();
    let cancellation_owner = RunCancellationOwner::new();
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn,
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::DirectTask,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: Some(plan_review_binding),
                task_handoff: None,
            },
        )))
        .with_cancellation(cancellation_owner.handle());
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let AgentRunDisposition::StartPlanReview(action) = output.disposition else {
        panic!(
            "expected StartPlanReview disposition, got {:?}",
            output.disposition
        );
    };
    assert_eq!(action.plan_review_id, expected_review_id);
    assert_eq!(action.source_turn.message_id, expected_message_id);

    let decisions = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(decision)) => {
                Some(decision.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].route, ConversationRoute::PlanReview);
    assert_eq!(
        decisions[0].reason_codes,
        vec![
            ConversationRouteReason::ArchitecturalTradeoff,
            ConversationRouteReason::ScopeUncertain
        ]
    );
    assert_eq!(
        decisions[0].effective_capability,
        AutomaticRouteCapability::ReviewFirst
    );
    assert!(session.entries().iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
    )));
    let captured_requests = captured.lock().expect("capture lock");
    assert_eq!(captured_requests.len(), 1);
    assert_eq!(captured_requests[0].tools.len(), 3);
    Ok(())
}

#[tokio::test]
async fn routing_microturn_executes_approved_memory_before_starting_plan_review() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let memory_executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RoutingMemoryTool {
        executed: Arc::clone(&memory_executed),
        project_scoped: false,
    }));
    registry.register(Arc::new(RoutingMemoryTool {
        executed: Arc::new(AtomicBool::new(false)),
        project_scoped: true,
    }));
    let agent = Agent::new(
        PlanReviewWithMemoryRoutingProvider {
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session = Session::new("mock-plan-review-with-memory", "mock-model");
    let prompt = "Remember that finish means commit, then propose the architecture first";
    let logical_run_id = "plan-review-memory-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let plan_review_binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let cancellation_owner = RunCancellationOwner::new();
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn,
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::DirectTask,
                writable_memory_routing: true,
                task_continuation: None,
                plan_review: Some(plan_review_binding),
                task_handoff: None,
            },
        )))
        .with_cancellation(cancellation_owner.handle());
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig {
                    enabled: false,
                    writable: true,
                },
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert!(matches!(
        output.disposition,
        AgentRunDisposition::StartPlanReview(_)
    ));
    assert!(memory_executed.load(Ordering::SeqCst));
    assert!(handler.events.iter().all(|event| {
        !matches!(
            event,
            RunEvent::ReasoningDelta(delta)
                if delta == "memory routing reasoning stays internal"
        ) && !matches!(
            event,
            RunEvent::TextDelta(delta) if delta == "memory routing narrative stays internal"
        )
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::ToolCallStarted(call) if call.id == "call-remember-routing"
        )
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::ToolResult(result) if result.call_id == "call-remember-routing"
        )
    }));
    let captured = captured.lock().expect("capture lock");
    let exposed = captured[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<BTreeSet<_>>();
    assert!(exposed.contains(crate::REMEMBER_USER_PREFERENCE_TOOL_NAME));
    assert!(exposed.contains(crate::REMEMBER_PROJECT_FACT_TOOL_NAME));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-remember-routing"
                    && approval.action == ToolApprovalAuditAction::Resolved
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.call_id == "call-remember-routing"
                    && execution.status == ToolExecutionStatus::Completed
        )
    }));
    let memory_execution_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.call_id == "call-remember-routing"
                        && execution.status == ToolExecutionStatus::Completed
            )
        })
        .expect("memory execution should be durable");
    let route_decision_index = session
        .entries()
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(_))
            )
        })
        .expect("route decision should be durable");
    assert!(memory_execution_index < route_decision_index);
    let result_order = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result)
                if matches!(
                    result.call_id.as_str(),
                    "call-plan-review-with-memory" | "call-remember-routing"
                ) =>
            {
                Some(result.call_id.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        result_order,
        vec!["call-plan-review-with-memory", "call-remember-routing"]
    );
    Ok(())
}

#[tokio::test]
async fn bound_writable_memory_routing_fails_closed_without_canonical_tools() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        PlanReviewRoutingProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-missing-routing-memory", "mock-model");
    let prompt = "remember this preference and review the architecture";
    let logical_run_id = "missing-routing-memory-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let plan_review_binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let cancellation_owner = RunCancellationOwner::new();
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn,
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::DirectTask,
                writable_memory_routing: true,
                task_continuation: None,
                plan_review: Some(plan_review_binding),
                task_handoff: None,
            },
        )))
        .with_cancellation(cancellation_owner.handle());
    let mut handler = crate::event::NoopEventHandler;

    let error = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig {
                    enabled: false,
                    writable: true,
                },
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("a bound writable-memory route must retain its canonical tools");

    assert!(
        error
            .to_string()
            .contains("writable memory routing requires registered tool remember_user_preference")
    );
    assert!(captured.lock().expect("capture lock").is_empty());
    Ok(())
}

#[tokio::test]
async fn manual_conversation_keeps_writable_memory_prompt_when_tools_are_available() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(RoutingMemoryTool {
        executed: Arc::new(AtomicBool::new(false)),
        project_scoped: false,
    }));
    registry.register(Arc::new(RoutingMemoryTool {
        executed: Arc::new(AtomicBool::new(false)),
        project_scoped: true,
    }));
    let agent = Agent::new(
        CapturingTextProvider {
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session = Session::new("mock-manual-memory", "mock-model");
    let logical_run_id = "manual-memory-run";
    let input = AgentRunInput::user("remember this for later");
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
                ConversationPurposeContext {
                    root_run_id: logical_run_id.to_owned(),
                    source_turn,
                    routing_policy: TaskRoutingPolicy::Manual,
                    route_capability: AutomaticRouteCapability::Unsupported,
                    writable_memory_routing: false,
                    task_continuation: None,
                    plan_review: None,
                    task_handoff: None,
                },
            )));
    let mut handler = crate::event::NoopEventHandler;

    agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig {
                    enabled: false,
                    writable: true,
                },
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    let captured = captured.lock().expect("capture lock");
    let request = captured.first().expect("one provider request");
    let system_prompt = request
        .messages
        .iter()
        .find(|message| message.id == "system:base")
        .and_then(|message| message.content.as_deref())
        .expect("base system prompt");
    assert!(system_prompt.contains("Writable memory is available"));
    assert!(!system_prompt.contains("Writable memory tools are unavailable"));
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == crate::REMEMBER_USER_PREFERENCE_TOOL_NAME)
    );
    assert!(
        request
            .tools
            .iter()
            .any(|tool| tool.name == crate::REMEMBER_PROJECT_FACT_TOOL_NAME)
    );
    Ok(())
}

#[tokio::test]
async fn review_first_capability_hides_the_direct_task_decision() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        PlanReviewRoutingProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-review-first-routing", "mock-model");
    let prompt = "design the migration path first";
    let logical_run_id = "review-first-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let plan_review_binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let cancellation_owner = RunCancellationOwner::new();
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn,
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::ReviewFirst,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: Some(plan_review_binding),
                task_handoff: None,
            },
        )))
        .with_cancellation(cancellation_owner.handle());
    let mut handler = crate::event::NoopEventHandler;

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::StartPlanReview(_)
    ));
    let captured_requests = captured.lock().expect("capture lock");
    let tool_names = captured_requests[0]
        .tools
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        tool_names,
        vec![
            crate::REQUEST_PLAN_REVIEW_TOOL_NAME,
            CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME
        ]
    );
    Ok(())
}

#[tokio::test]
async fn chat_decision_records_route_decision_without_effect_authority() -> Result<()> {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        ChatDecisionRoutingProvider {
            captured: Arc::clone(&captured),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-chat-routing", "mock-model");
    let prompt = "explain how the queue promotion works";
    let logical_run_id = "chat-routing-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let plan_review_binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let cancellation_owner = RunCancellationOwner::new();
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn,
                routing_policy: TaskRoutingPolicy::Auto,
                route_capability: AutomaticRouteCapability::ReviewFirst,
                writable_memory_routing: false,
                task_continuation: None,
                plan_review: Some(plan_review_binding),
                task_handoff: None,
            },
        )))
        .with_cancellation(cancellation_owner.handle());
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(
            &mut session,
            input,
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(3),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::FinalAnswer
    ));
    let decisions = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(decision)) => {
                Some(decision.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].route, ConversationRoute::Chat);
    assert!(decisions[0].reason_codes.is_empty());
    assert!(session.entries().iter().all(|entry| !matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::TaskHandoffRequested(_))
    )));
    let captured_requests = captured.lock().expect("capture lock");
    assert_eq!(captured_requests.len(), 2);
    let reasoning = handler
        .events
        .iter()
        .filter_map(|event| match event {
            RunEvent::ReasoningDelta(delta) => Some(delta.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(reasoning, vec!["work reasoning is visible"]);
    assert!(handler.events.iter().all(|event| {
        !matches!(event, RunEvent::TextDelta(delta) if delta == "internal routing narrative")
    }));
    assert!(handler.events.iter().all(|event| {
        !matches!(
            event,
            RunEvent::ToolCallStarted(call)
                | RunEvent::ToolCallCompleted(call)
                if call.id == "call-chat-decision"
        ) && !matches!(
            event,
            RunEvent::ToolResult(result) if result.call_id == "call-chat-decision"
        )
    }));
    Ok(())
}

struct TaskHandoffProvider;
struct TaskContinuationProvider;
struct TaskHandoffSideEffectTool {
    executions: Arc<AtomicUsize>,
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
impl Provider for GuidanceDecisionProvider {
    fn name(&self) -> &str {
        "mock-guidance-decision"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.observed_tools
            .lock()
            .expect("observed tools lock should not be poisoned")
            .extend(request.tools.iter().map(|tool| tool.name.clone()));
        let (name, args) = match self.decision {
            GuidanceDecision::Apply => (
                TASK_GUIDANCE_APPLY_TOOL_NAME,
                r#"{"reason":"prioritizes_pending_step","target_step_ids":["step_1"]}"#,
            ),
            GuidanceDecision::Replan => (
                TASK_PLAN_UPDATE_TOOL_NAME,
                r#"{"plan_version":3,"status":"accepted","steps":[{"step_id":"step_1","title":"inspect","role":"executor"},{"step_id":"step_2","title":"security review","role":"subagent_read","depends_on":["step_1"]}],"reason":"guidance changes required steps"}"#,
            ),
        };
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-guidance-decision".to_owned(),
                name: name.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-guidance-decision".to_owned(),
                delta: args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-guidance-decision".to_owned(),
                name: name.to_owned(),
                args_json: args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for TaskHandoffProvider {
    fn name(&self) -> &str {
        "mock-task-handoff"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let handoff_args = r#"{"reason_codes":["cross_layer","multi_stage_change"]}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-side-effect".to_owned(),
                name: "handoff_side_effect".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-side-effect".to_owned(),
                delta: "{}".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-side-effect".to_owned(),
                name: "handoff_side_effect".to_owned(),
                args_json: "{}".to_owned(),
            })),
            Ok(ProviderChunk::ToolCallStart {
                id: "call-handoff-1".to_owned(),
                name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-handoff-1".to_owned(),
                delta: handoff_args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-handoff-1".to_owned(),
                name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
                args_json: handoff_args.to_owned(),
            })),
            Ok(ProviderChunk::ToolCallStart {
                id: "call-handoff-2".to_owned(),
                name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-handoff-2".to_owned(),
                delta: handoff_args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-handoff-2".to_owned(),
                name: REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
                args_json: handoff_args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for TaskContinuationProvider {
    fn name(&self) -> &str {
        "mock-task-continuation"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let continuation_args =
            r#"{"reason":"continue_current_task","action":"apply_current_request_as_guidance"}"#;
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "call-side-effect-before-continuation".to_owned(),
                name: "handoff_side_effect".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-side-effect-before-continuation".to_owned(),
                delta: "{}".to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-side-effect-before-continuation".to_owned(),
                name: "handoff_side_effect".to_owned(),
                args_json: "{}".to_owned(),
            })),
            Ok(ProviderChunk::ToolCallStart {
                id: "call-continue-existing-task".to_owned(),
                name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallArgsDelta {
                id: "call-continue-existing-task".to_owned(),
                delta: continuation_args.to_owned(),
            }),
            Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id: "call-continue-existing-task".to_owned(),
                name: CONTINUE_EXISTING_TASK_TOOL_NAME.to_owned(),
                args_json: continuation_args.to_owned(),
            })),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Tool for TaskHandoffSideEffectTool {
    fn spec(&self) -> crate::ToolSpec {
        crate::ToolSpec {
            name: "handoff_side_effect".to_owned(),
            description: "must be ignored after accepted handoff".to_owned(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
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
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::ok(
            call_id,
            "handoff_side_effect",
            "executed",
            ToolResultMeta::default(),
        ))
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
        title: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                    && content.contains(r#""summary":"path_sha256="#)
                    && !content.contains("file.txt")
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
async fn expired_approval_has_no_user_decision_receipt_and_never_executes() -> Result<()> {
    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(WriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut handler = crate::event::NoopEventHandler;
    let mut approval_handler = ExpiredApprovalHandler;

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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::DecisionAccepted
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-write-1"
                    && approval.action == ToolApprovalAuditAction::Resolved
                    && approval.user_decision.is_none()
                    && approval.decision_receipt.is_none()
                    && approval.terminal_status
                        == Some(crate::ToolApprovalTerminalStatusV2::Expired)
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

#[cfg(unix)]
#[tokio::test]
async fn execution_replan_rejects_symlink_retargeted_to_protected_path_after_approval() -> Result<()>
{
    let workspace = tempfile::tempdir()?;
    let ordinary_target = workspace.path().join("ordinary.txt");
    std::fs::write(&ordinary_target, "ordinary")?;
    let protected_directory = workspace.path().join(".git");
    std::fs::create_dir(&protected_directory)?;
    let protected_target = protected_directory.join("config");
    std::fs::write(&protected_target, "protected")?;
    let link = workspace.path().join("file.txt");
    symlink(&ordinary_target, &link)?;

    let executed = Arc::new(AtomicBool::new(false));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(SymlinkWriteTool {
        executed: Arc::clone(&executed),
    }));
    let agent = Agent::new(WriteMockProvider, registry);
    let mut session = Session::new("mock-write", "mock-model");
    let mut event_handler = RecordingEventHandler::default();
    let mut approval_handler = RetargetSymlinkApprovalHandler {
        link,
        protected_target: protected_target.clone(),
    };

    let output = agent
        .run_with_approval_input(
            &mut session,
            AgentRunInput::user("write something"),
            AgentRunOptions {
                workspace_root: workspace.path().to_path_buf(),
                max_turns: Some(4),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut event_handler,
            &mut approval_handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert!(!executed.load(Ordering::SeqCst));
    assert_eq!(std::fs::read_to_string(protected_target)?, "protected");
    assert!(output.outcome.tool_errors.iter().any(|error| {
        error.kind == ToolErrorKind::StalePreparedMutation
            && error
                .message
                .contains("subjects or trust zones changed after authorization")
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
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
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
                if grant.source_call_id == "call-read-1"
                    && grant.tool_name == "read_path"
                    && grant.subjects.len() == 1
                    && grant.subjects[0].relative_label.as_deref() == Some("file.txt")
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
            SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(decision))
                if decision.call_id == "call-read-2"
                    && decision.policy_decision == ApprovalMode::Allow
                    && decision.allow_source == Some(ToolApprovalAllowSource::SessionGrant)
                    && decision.grant_id.is_some()
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
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
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
        permission_mode_override: None,
        permission_context: crate::PermissionEvaluationContext::default(),
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
    };
    let mut session = Session::load_from_store(
        "mock-session-grant-cargo-check",
        "mock-model",
        store.clone(),
    )?;
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
    drop(session);
    let mut session =
        Session::load_from_store("mock-session-grant-cargo-check", "mock-model", store)?;
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
                if grant.source_call_id == "call-cargo-1"
                    && grant.tool_name == "bash"
                    && grant.policy_version.starts_with("session-scope-sha256:")
                    && grant.subjects.len() == 1
                    && grant.subjects[0].identity_sha256
                        == crate::stable_event_hash(b"family:cargo_check")
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
            SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(decision))
                if decision.call_id == "call-cargo-2"
                    && decision.policy_decision == ApprovalMode::Allow
                    && decision.allow_source == Some(ToolApprovalAllowSource::SessionGrant)
                    && decision.grant_id.is_some()
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
    let accepted_index = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                    if approval.call_id == "call-write-1"
                        && approval.action == ToolApprovalAuditAction::DecisionAccepted
                        && approval.decision_receipt.as_ref().is_some_and(|receipt| {
                            receipt.approval_request_id
                                == approval.identity.approval_request_id
                        })
            )
        })
        .expect("approval acceptance receipt should be durable");
    let resolved_index = entries
        .iter()
        .position(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                    if approval.call_id == "call-write-1"
                        && approval.action == ToolApprovalAuditAction::Resolved
                        && approval.terminal_status
                            == Some(crate::ToolApprovalTerminalStatusV2::Approved)
            )
        })
        .expect("approval terminal status should be durable");

    assert!(snapshot_index < requested_index);
    assert!(requested_index < accepted_index);
    assert!(accepted_index < resolved_index);
    assert!(resolved_index < started_index);
    assert!(handler.events.iter().any(|event| {
        matches!(event, RunEvent::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-write-1"
                && execution.status == ToolExecutionStatus::Started)
    }));

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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                    && approval.user_decision.is_none()
                    && approval.terminal_status
                        == Some(crate::ToolApprovalTerminalStatusV2::Stale)
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

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
            SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(decision))
                if decision.call_id == "call-write-1"
                    && decision.policy_decision == ApprovalMode::Ask
                    && decision.local_policy_decision == ApprovalMode::Ask
                    && decision.source_policy_decision == ApprovalMode::Allow
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

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
            SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(decision))
                if decision.call_id == "call-write-1"
                    && decision.policy_decision == ApprovalMode::Deny
                    && decision.local_policy_decision == ApprovalMode::Deny
        )
    }));
    assert!(handler.events.iter().any(|event| {
        matches!(
            event,
            RunEvent::ToolResult(result)
                if result.is_error()
                    && result.content.contains("denied by permission policy")
        )
    }));
    Ok(())
}

#[tokio::test]
async fn agent_requests_approval_for_external_directory_when_disabled_interactive() -> Result<()> {
    let external_path = synthetic_external_test_root()?.join("outside.txt");
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
    let external_path = synthetic_external_test_root()?.join("outside.txt");
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
    let external_path = synthetic_external_test_root()?.join("outside.txt");
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

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
    let external_root = synthetic_external_test_root()?;
    let external_rule_glob = external_root
        .parent()
        .map(|root| root.join("**").display().to_string())
        .ok_or_else(|| std::io::Error::other("external test path has no filesystem root"))?;
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
                            path_glob: external_rule_glob,
                            mode: ApprovalMode::Allow,
                        }],
                        ..ExternalDirectoryConfig::default()
                    },
                    ..PermissionConfig::default()
                },

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
    crate::session::append_current_test_session_identity(&store)?;
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                        .is_some_and(|summary| summary.starts_with("path_sha256="))
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
struct TypedProtocolViolationProvider;

#[derive(Debug, thiserror::Error)]
#[error("transport connect failed before request dispatch")]
struct ConnectFailedBeforeDispatch;

struct ConnectRetryProvider {
    connect_failures: usize,
    calls: Arc<AtomicUsize>,
}

struct UnclassifiedPreStreamErrorProvider {
    calls: Arc<AtomicUsize>,
}

/// Deterministic stand-in for a TLS EOF after request bytes may have been sent. The adapter
/// owns the typed mapping; recovery must not inspect the error text.
struct TlsCloseThenSuccessProvider {
    failures: usize,
    calls: Arc<AtomicUsize>,
}

struct PartialStreamThenSuccessProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

/// A streamed tool-request prefix is not replay-safe, even when the adapter subsequently fails
/// before the tool call can become a settled local effect.
struct PartialToolRequestThenErrorProvider {
    calls: Arc<AtomicUsize>,
}

/// Test-only route with two provider-declared equivalent transports. It starts on the primary
/// transport and can move only after the durable fallback selection event is appended.
struct EquivalentTransportFallbackProvider {
    calls: Arc<AtomicUsize>,
    fallback_active: Arc<AtomicBool>,
}

/// Deterministic stand-in for typed 429/5xx provider failures. The stream text is deliberately
/// uninformative: recovery must follow the provider-owned typed observation rather than parsing
/// error strings.
struct ClassifiedFailureThenSuccessProvider {
    calls: Arc<AtomicUsize>,
    failure_class: ProviderFailureClassV1,
}

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

#[async_trait]
impl Provider for TypedProtocolViolationProvider {
    fn name(&self) -> &str {
        "mock-typed-protocol-violation"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        Ok(Box::pin(stream::iter(vec![Err(
            crate::ProviderProtocolViolation::UnstructuredToolInvocation.into(),
        )])))
    }
}

#[async_trait]
impl Provider for ConnectRetryProvider {
    fn name(&self) -> &str {
        "mock-connect-retry"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn classify_pre_generation_rejection(
        &self,
        error: &anyhow::Error,
    ) -> Option<ProviderRequestRejection> {
        error
            .downcast_ref::<ConnectFailedBeforeDispatch>()
            .is_some()
            .then_some(ProviderRequestRejection::ConnectFailedBeforeDispatch)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_index < self.connect_failures {
            return Err(ConnectFailedBeforeDispatch.into());
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for UnclassifiedPreStreamErrorProvider {
    fn name(&self) -> &str {
        "mock-unclassified-pre-stream-error"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        anyhow::bail!("transport outcome is uncertain")
    }
}

#[async_trait]
impl Provider for TlsCloseThenSuccessProvider {
    fn name(&self) -> &str {
        "mock-tls-close"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn observe_failure(
        &self,
        _error: &anyhow::Error,
        wire_state: ProviderWireStateV1,
    ) -> ProviderFailureObservationV1 {
        ProviderFailureObservationV1::transport_interrupted(wire_state)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_index < self.failures {
            anyhow::bail!("peer closed connection without sending TLS close_notify");
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("done".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for PartialStreamThenSuccessProvider {
    fn name(&self) -> &str {
        "mock-partial-stream"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn observe_failure(
        &self,
        _error: &anyhow::Error,
        wire_state: ProviderWireStateV1,
    ) -> ProviderFailureObservationV1 {
        ProviderFailureObservationV1::transport_interrupted(wire_state)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.requests
            .lock()
            .expect("partial-stream request capture lock should not be poisoned")
            .push(request);
        let call_index = self.calls.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta(
                    "discard this partial answer".to_owned(),
                )),
                Err(anyhow::anyhow!(
                    "deterministic tls eof after response start"
                )),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta("recovered answer".to_owned())),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for PartialToolRequestThenErrorProvider {
    fn name(&self) -> &str {
        "mock-partial-tool-request"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn observe_failure(
        &self,
        _error: &anyhow::Error,
        wire_state: ProviderWireStateV1,
    ) -> ProviderFailureObservationV1 {
        ProviderFailureObservationV1::transport_interrupted(wire_state)
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::ToolCallStart {
                id: "partial-tool-call".to_owned(),
                name: "write_file".to_owned(),
            }),
            Err(anyhow::anyhow!(
                "deterministic tls eof after streamed tool request"
            )),
        ])))
    }
}

#[async_trait]
impl Provider for EquivalentTransportFallbackProvider {
    fn name(&self) -> &str {
        "mock-equivalent-transport"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn observe_failure(
        &self,
        _error: &anyhow::Error,
        wire_state: ProviderWireStateV1,
    ) -> ProviderFailureObservationV1 {
        ProviderFailureObservationV1::transport_interrupted(wire_state)
    }

    fn transport_fallback_candidate(
        &self,
        _request: &CompletionRequest,
        _failure: &ProviderFailureObservationV1,
    ) -> Option<crate::ProviderTransportFallbackCandidateV1> {
        (!self.fallback_active.load(Ordering::SeqCst)).then(|| {
            crate::ProviderTransportFallbackCandidateV1 {
                fallback_transport_id: "https".to_owned(),
                source_transport_fingerprint: format!("sha256:{}", "a".repeat(64)),
                fallback_transport_fingerprint: format!("sha256:{}", "b".repeat(64)),
                semantic_route_fingerprint: format!("sha256:{}", "c".repeat(64)),
            }
        })
    }

    fn activate_transport_fallback(
        &self,
        candidate: &crate::ProviderTransportFallbackCandidateV1,
    ) -> Result<()> {
        candidate.validate()?;
        anyhow::ensure!(
            candidate.fallback_transport_id == "https",
            "unexpected fallback transport"
        );
        self.fallback_active.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !self.fallback_active.load(Ordering::SeqCst) {
            anyhow::bail!("deterministic primary transport TLS EOF")
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "fallback transport answer".to_owned(),
            )),
            Ok(ProviderChunk::Done),
        ])))
    }
}

#[async_trait]
impl Provider for ClassifiedFailureThenSuccessProvider {
    fn name(&self) -> &str {
        "mock-classified-provider-failure"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        StreamErrorProvider.capabilities()
    }

    fn observe_failure(
        &self,
        _error: &anyhow::Error,
        wire_state: ProviderWireStateV1,
    ) -> ProviderFailureObservationV1 {
        ProviderFailureObservationV1::classified(
            self.failure_class,
            wire_state,
            "deterministic_classified_provider_failure",
        )
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            anyhow::bail!("opaque fixture failure")
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(
                "classified recovery answer".to_owned(),
            )),
            Ok(ProviderChunk::Done),
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
async fn agent_returns_invalid_input_when_permission_plan_fails() -> Result<()> {
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
async fn agent_retries_confirmed_pre_dispatch_connect_failures_with_frozen_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        ConnectRetryProvider {
            connect_failures: 2,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-connect-retry", "mock-model").with_store(store);
    let mut handler = RecordingEventHandler::default();

    let output = agent
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.final_text, "done");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(
        handler
            .events
            .iter()
            .filter(|event| matches!(event, RunEvent::Notice(message) if message.contains("Reconnecting...")))
            .count(),
        2
    );

    let records = JsonlSessionStore::read_event_records(&path)?;
    let started = records
        .iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str() =>
            {
                serde_json::from_value::<ProviderPhysicalAttemptStartedEntry>(event.payload.clone())
                    .ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(started.len(), 3);
    assert!(
        started
            .iter()
            .all(|entry| entry.logical_run_id == started[0].logical_run_id)
    );

    let recovery = session.provider_turn_recovery_projection()?;
    assert_eq!(
        recovery
            .recoveries_for_logical_run_id(&started[0].logical_run_id)
            .len(),
        2
    );
    assert!(
        started
            .iter()
            .all(|entry| entry.request_material_fingerprint
                == started[0].request_material_fingerprint)
    );

    let projection = session.provider_physical_attempt_projection()?;
    let attempts = projection.attempts_for_logical_run_id(&started[0].logical_run_id);
    assert_eq!(attempts.len(), 3);
    for attempt in &attempts[..2] {
        assert!(matches!(
            attempt.terminal.as_ref(),
            Some(ProviderPhysicalAttemptTerminalEntry {
                outcome: ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
                rejection: Some(ProviderRequestRejection::ConnectFailedBeforeDispatch),
                ..
            })
        ));
    }
    assert!(matches!(
        attempts[2].terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            rejection: None,
            ..
        })
    ));
    assert_eq!(
        projection
            .effective_attempt_for_logical_run_id(&started[0].logical_run_id)?
            .map(|attempt| attempt.entry.physical_attempt_id.as_str()),
        Some(attempts[2].entry.physical_attempt_id.as_str())
    );
    Ok(())
}

#[tokio::test]
async fn agent_recovers_zero_effect_tls_close_in_same_logical_turn() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TlsCloseThenSuccessProvider {
            failures: 1,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session =
        Session::new("mock-tls-close", "mock-model").with_store(JsonlSessionStore::new(&path)?);
    let mut handler = RecordingEventHandler::default();

    let output = agent
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await?;

    assert_eq!(output.final_text, "done");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(handler.events.iter().any(
        |event| matches!(event, RunEvent::Notice(message) if message == "Reconnecting... 1/2")
    ));
    assert!(handler.events.iter().any(|event| matches!(
        event,
        RunEvent::ProviderTurnRecovery(view)
            if view.phase == crate::PublicProviderTurnRecoveryPhaseV1::Waiting
                && view.retry_count == 1
                && view.active_retry_count == 1
                && view.active_max_retries == 2
                && view.max_transport_retries == 2
                && !view.user_attention_required
    )));
    assert!(handler.events.iter().any(|event| matches!(
        event,
        RunEvent::ProviderTurnRecovery(view)
            if view.phase == crate::PublicProviderTurnRecoveryPhaseV1::Recovering
                && view.retry_count == 1
    )));

    let records = JsonlSessionStore::read_event_records(&path)?;
    let attempts = ProviderPhysicalAttemptProjection::from_records(&records)?;
    let logical_run_id = attempts
        .attempts()
        .first()
        .expect("the initial physical attempt should be present")
        .entry
        .logical_run_id
        .clone();
    let chain = attempts.attempts_for_logical_run_id(&logical_run_id);
    assert_eq!(chain.len(), 2);
    assert!(matches!(
        chain[0].terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            durable_output_event_ids,
            durable_side_effect_event_ids,
            ..
        }) if durable_output_event_ids.is_empty() && durable_side_effect_event_ids.is_empty()
    ));
    assert!(matches!(
        chain[1].terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::Completed,
            ..
        })
    ));
    assert_eq!(
        attempts
            .effective_attempt_for_logical_run_id(&logical_run_id)?
            .map(|attempt| attempt.entry.physical_attempt_id.as_str()),
        Some(chain[1].entry.physical_attempt_id.as_str())
    );

    let recovery = session.provider_turn_recovery_projection()?;
    let schedules = recovery.recoveries_for_logical_run_id(&logical_run_id);
    assert_eq!(schedules.len(), 1);
    assert!(schedules[0].started.is_some());
    assert!(
        recovery
            .terminal_for_logical_run_id(&logical_run_id)
            .is_none()
    );
    assert!(!records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str()
                && event.payload.get("run_status").and_then(Value::as_str) == Some("failed")
    )));
    Ok(())
}

#[tokio::test]
async fn agent_recovers_typed_rate_limit_and_transient_server_failures() -> Result<()> {
    let policy = ProviderTurnRecoveryPolicyV1 {
        max_transport_retries: 1,
        max_partial_output_retries: 0,
        initial_delay_ms: 0,
        max_delay_ms: 0,
        jitter_ratio_millionths: 0,
        max_cumulative_delay_ms: 0,
    };
    for failure_class in [
        ProviderFailureClassV1::RateLimited,
        ProviderFailureClassV1::TransientServer,
    ] {
        let temp = tempfile::tempdir()?;
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = Agent::new(
            ClassifiedFailureThenSuccessProvider {
                calls: Arc::clone(&calls),
                failure_class,
            },
            ToolRegistry::new(),
        )
        .with_provider_turn_recovery_policy(policy)?;
        let mut session = Session::new("mock-classified-provider-failure", "mock-model")
            .with_store(JsonlSessionStore::new(temp.path().join("session.jsonl"))?);
        let mut handler = RecordingEventHandler::default();

        let output = agent
            .run(
                &mut session,
                "recover typed failure",
                scripted_run_options(1),
                &mut handler,
            )
            .await?;

        assert_eq!(output.final_text, "classified recovery answer");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let attempts = session.provider_physical_attempt_projection()?;
        let logical_run_id = attempts
            .attempts()
            .first()
            .expect("typed failure should have an initial attempt")
            .entry
            .logical_run_id
            .clone();
        assert_eq!(
            attempts.attempts_for_logical_run_id(&logical_run_id).len(),
            2
        );
        let recovery = session.provider_turn_recovery_projection()?;
        assert!(
            recovery
                .recoveries_for_logical_run_id(&logical_run_id)
                .iter()
                .any(|state| state.schedule.failure_class == failure_class)
        );
    }
    Ok(())
}

#[tokio::test]
async fn agent_blocks_typed_authentication_and_protocol_failures_without_resend() -> Result<()> {
    for (failure_class, expected_reason) in [
        (
            ProviderFailureClassV1::Authentication,
            "provider_configuration_or_capacity_required",
        ),
        (
            ProviderFailureClassV1::ProtocolViolation,
            "provider_request_requires_attention",
        ),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = Agent::new(
            ClassifiedFailureThenSuccessProvider {
                calls: Arc::clone(&calls),
                failure_class,
            },
            ToolRegistry::new(),
        );
        let temp = tempfile::tempdir()?;
        let mut session = Session::new("mock-classified-provider-failure", "mock-model")
            .with_store(JsonlSessionStore::new(temp.path().join("session.jsonl"))?);
        let mut handler = RecordingEventHandler::default();

        let _error = agent
            .run(
                &mut session,
                "do not resend blocked failure",
                scripted_run_options(1),
                &mut handler,
            )
            .await
            .expect_err("typed non-transient failures must not be replayed");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let logical_run_id = session
            .provider_physical_attempt_projection()?
            .attempts()
            .first()
            .expect("blocked failure should have one attempt")
            .entry
            .logical_run_id
            .clone();
        let recovery = session.provider_turn_recovery_projection()?;
        assert!(
            recovery
                .recoveries_for_logical_run_id(&logical_run_id)
                .is_empty()
        );
        assert!(
            recovery
                .terminal_for_logical_run_id(&logical_run_id)
                .is_some_and(|terminal| terminal.reason_code == expected_reason)
        );
    }
    Ok(())
}

#[tokio::test]
async fn agent_discards_partial_stream_before_bounded_retry_and_replaces_live_output() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let agent = Agent::new(
        PartialStreamThenSuccessProvider {
            calls: Arc::clone(&calls),
            requests: Arc::clone(&requests),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-partial-stream", "mock-model")
        .with_store(JsonlSessionStore::new(&path)?);
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run(&mut session, "hi", scripted_run_options(1), &mut handler)
        .await?;

    assert_eq!(output.final_text, "recovered answer");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(handler.events.iter().any(|event| matches!(
        event,
        RunEvent::ProviderTurnPartialOutputDiscarded(view)
            if view.text_discarded
                && !view.reasoning_discarded
                && !view.tool_request_discarded
    )));
    let requests = requests
        .lock()
        .expect("partial-stream request capture should remain available");
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().all(|message| {
        message
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("discard this partial answer"))
    }));
    assert!(session.messages().iter().all(|message| {
        message
            .content
            .as_deref()
            .is_none_or(|content| !content.contains("discard this partial answer"))
    }));

    let records = JsonlSessionStore::read_event_records(&path)?;
    let attempts = ProviderPhysicalAttemptProjection::from_records(&records)?;
    let logical_run_id = attempts
        .attempts()
        .first()
        .expect("partial-stream attempt must be durable")
        .entry
        .logical_run_id
        .clone();
    let chain = attempts.attempts_for_logical_run_id(&logical_run_id);
    assert_eq!(chain.len(), 2);
    let recovery = session.provider_turn_recovery_projection()?;
    assert!(
        recovery
            .discarded_partial_for_physical_attempt(&chain[0].entry.physical_attempt_id)
            .is_some()
    );
    let schedule = recovery
        .recoveries_for_logical_run_id(&logical_run_id)
        .into_iter()
        .next()
        .expect("partial-stream retry must be scheduled durably");
    assert_eq!(
        schedule.schedule.retry_kind,
        crate::ProviderTurnRecoveryRetryKindV1::PartialOutput
    );
    assert_eq!(
        schedule.schedule.budget_snapshot.partial_output_retry_count,
        1
    );
    assert!(handler.events.iter().any(|event| matches!(
        event,
        RunEvent::ProviderTurnRecovery(view)
            if view.phase == crate::PublicProviderTurnRecoveryPhaseV1::Waiting
                && view.active_retry_count == 1
                && view.active_max_retries == 1
    )));
    Ok(())
}

#[tokio::test]
async fn agent_blocks_partial_tool_request_without_replaying_provider_or_tool_effect() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        PartialToolRequestThenErrorProvider {
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-partial-tool-request", "mock-model")
        .with_store(JsonlSessionStore::new(&path)?);
    let mut handler = RecordingEventHandler::default();

    let error = agent
        .run(&mut session, "hi", scripted_run_options(1), &mut handler)
        .await
        .expect_err("a partial tool request must require review rather than replay");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(
        error
            .downcast_ref::<crate::ProviderTurnRecoveryTerminalError>()
            .is_some_and(|terminal| {
                terminal.disposition == crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked
                    && terminal.reason_code == "partial_provider_tool_request_requires_review"
            })
    );
    assert!(handler.events.iter().any(|event| matches!(
        event,
        RunEvent::ProviderTurnPartialOutputDiscarded(view)
            if !view.text_discarded
                && !view.reasoning_discarded
                && view.tool_request_discarded
    )));

    let attempts = session.provider_physical_attempt_projection()?;
    let logical_run_id = attempts
        .attempts()
        .first()
        .expect("partial tool attempt must be durable")
        .entry
        .logical_run_id
        .clone();
    assert_eq!(
        attempts.attempts_for_logical_run_id(&logical_run_id).len(),
        1
    );
    let recovery = session.provider_turn_recovery_projection()?;
    assert!(
        recovery
            .recoveries_for_logical_run_id(&logical_run_id)
            .is_empty()
    );
    assert!(matches!(
        recovery.terminal_for_logical_run_id(&logical_run_id),
        Some(terminal)
            if terminal.reason_code == "partial_provider_tool_request_requires_review"
                && terminal.terminal_disposition
                    == crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked
    ));
    assert!(
        session
            .messages()
            .iter()
            .all(|message| message.role != MessageRole::Tool)
    );
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(!records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str()
                && event.payload.get("run_status").and_then(Value::as_str) == Some("failed")
    )));
    Ok(())
}

#[tokio::test]
async fn agent_selects_equivalent_transport_only_after_durable_recovery_authority() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let calls = Arc::new(AtomicUsize::new(0));
    let fallback_active = Arc::new(AtomicBool::new(false));
    let agent = Agent::new(
        EquivalentTransportFallbackProvider {
            calls: Arc::clone(&calls),
            fallback_active: Arc::clone(&fallback_active),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-equivalent-transport", "mock-model")
        .with_store(JsonlSessionStore::new(&path)?);
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run(&mut session, "hi", scripted_run_options(1), &mut handler)
        .await?;

    assert_eq!(output.final_text, "fallback transport answer");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(fallback_active.load(Ordering::SeqCst));
    let recovery = session.provider_turn_recovery_projection()?;
    let state = recovery
        .recoveries_for_logical_run_id(
            &session
                .provider_physical_attempt_projection()?
                .attempts()
                .first()
                .expect("primary attempt should be durable")
                .entry
                .logical_run_id,
        )
        .into_iter()
        .next()
        .expect("fallback recovery should be durable");
    assert_eq!(
        state
            .transport_fallback
            .as_ref()
            .map(|selection| selection.candidate.fallback_transport_id.as_str()),
        Some("https")
    );
    let records = JsonlSessionStore::read_event_records(&path)?;
    let fallback_position = records
        .iter()
        .position(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type
                        == DurableEventType::ProviderTurnTransportFallbackSelected.as_str()
            )
        })
        .expect("fallback selection must be durable");
    let start_position = records
        .iter()
        .position(|record| {
            matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type == DurableEventType::ProviderTurnRecoveryStarted.as_str()
            )
        })
        .expect("recovery must start durably");
    assert!(fallback_position < start_position);
    assert!(!records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str()
                && event.payload.get("run_status").and_then(Value::as_str) == Some("failed")
    )));
    Ok(())
}

#[tokio::test]
async fn agent_repeats_deterministic_tls_recovery_without_duplicate_terminal_events() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let policy = ProviderTurnRecoveryPolicyV1 {
        max_transport_retries: 1,
        max_partial_output_retries: 0,
        initial_delay_ms: 0,
        max_delay_ms: 0,
        jitter_ratio_millionths: 0,
        max_cumulative_delay_ms: 0,
    };
    for run_index in 0..100 {
        let path = temp.path().join(format!("tls-recovery-{run_index}.jsonl"));
        let calls = Arc::new(AtomicUsize::new(0));
        let agent = Agent::new(
            TlsCloseThenSuccessProvider {
                failures: 1,
                calls: Arc::clone(&calls),
            },
            ToolRegistry::new(),
        )
        .with_provider_turn_recovery_policy(policy)?;
        let mut session =
            Session::new("mock-tls-close", "mock-model").with_store(JsonlSessionStore::new(&path)?);
        let mut handler = RecordingEventHandler::default();

        let output = agent
            .run(
                &mut session,
                "recover deterministically",
                scripted_run_options(1),
                &mut handler,
            )
            .await?;

        assert_eq!(output.final_text, "done", "iteration {run_index}");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "iteration {run_index}");
        let attempts = session.provider_physical_attempt_projection()?;
        assert_eq!(attempts.attempts().len(), 2, "iteration {run_index}");
        assert!(
            attempts
                .attempts()
                .iter()
                .all(|attempt| attempt.terminal.is_some()),
            "iteration {run_index}"
        );
        assert!(
            session
                .provider_turn_recovery_projection()?
                .recoveries_for_logical_run_id(
                    &attempts
                        .attempts()
                        .first()
                        .expect("retry fixture must retain the logical attempt")
                        .entry
                        .logical_run_id,
                )
                .iter()
                .all(|state| state.schedule.recovery_policy_fingerprint == policy.fingerprint()),
            "iteration {run_index} must retain the configured policy snapshot"
        );
        let records = JsonlSessionStore::read_event_records(&path)?;
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record,
                    SessionStreamRecord::Stored(event)
                        if event.event_type == DurableEventType::ProviderTurnRecoveryScheduled.as_str()
                ))
                .count(),
            1,
            "iteration {run_index}"
        );
        assert_eq!(
            records
                .iter()
                .filter(|record| matches!(
                    record,
                    SessionStreamRecord::Stored(event)
                        if event.event_type == DurableEventType::ProviderTurnRecoveryStarted.as_str()
                ))
                .count(),
            1,
            "iteration {run_index}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn agent_restores_durable_transport_fallback_after_session_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let logical_run_id = "fallback-restart-logical-turn";
    let mut session = Session::new("mock-equivalent-transport", "mock-model")
        .with_store(JsonlSessionStore::new(&path)?);
    session.ensure_identity_entry()?;
    let mut durable_user = ModelMessage::user("continue over the selected equivalent transport");
    durable_user.id = "fallback-restart-user".to_owned();
    session.append_user_message(durable_user.clone())?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-equivalent-transport".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: vec![durable_user],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: Some(ReasoningEffort::Medium),
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let mut original =
        crate::session::ProviderPhysicalAttemptAudit::start(&session, logical_run_id, &frozen)
            .await?;
    let original_id = original
        .physical_attempt_id()
        .expect("durable original physical attempt")
        .to_owned();
    original
        .finish(
            &session,
            ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            None,
        )
        .await?;
    let evidence = ProviderTurnRecoveryEvidenceV1::from_terminal_attempt(
        session
            .provider_physical_attempt_projection()?
            .attempt(&original_id)
            .expect("the original attempt must be projected"),
        ProviderFailureObservationV1::transport_interrupted(
            ProviderWireStateV1::RequestBytesMayHaveBeenSent,
        ),
        &frozen,
    )?;
    let schedule = crate::session::ProviderTurnRecoveryAudit::schedule(
        &session,
        &evidence,
        Default::default(),
        0,
        ProviderTurnRecoveryPolicyV1::default(),
    )
    .await?;
    crate::session::ProviderTurnRecoveryAudit::select_transport_fallback(
        &session,
        &schedule,
        crate::ProviderTransportFallbackCandidateV1 {
            fallback_transport_id: "https".to_owned(),
            source_transport_fingerprint: format!("sha256:{}", "a".repeat(64)),
            fallback_transport_fingerprint: format!("sha256:{}", "b".repeat(64)),
            semantic_route_fingerprint: format!("sha256:{}", "c".repeat(64)),
        },
    )
    .await?;
    drop(session);

    let mut resumed = Session::load_from_store(
        "mock-equivalent-transport",
        "mock-model",
        JsonlSessionStore::new(&path)?,
    )?;
    let calls = Arc::new(AtomicUsize::new(0));
    let fallback_active = Arc::new(AtomicBool::new(false));
    let agent = Agent::new(
        EquivalentTransportFallbackProvider {
            calls: Arc::clone(&calls),
            fallback_active: Arc::clone(&fallback_active),
        },
        ToolRegistry::new(),
    );
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(
            &mut resumed,
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen)
                .with_logical_run_id(logical_run_id),
            scripted_run_options(1),
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "fallback transport answer");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(fallback_active.load(Ordering::SeqCst));
    let recovery = resumed.provider_turn_recovery_projection()?;
    let state = recovery
        .recovery(&schedule.recovery_id)
        .expect("the selected recovery must survive reload");
    assert!(state.started.is_some());
    assert_eq!(
        state
            .transport_fallback
            .as_ref()
            .map(|selection| selection.candidate.fallback_transport_id.as_str()),
        Some("https")
    );
    assert_eq!(
        JsonlSessionStore::read_event_records(&path)?
            .iter()
            .filter(|record| matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type
                        == DurableEventType::ProviderTurnTransportFallbackSelected.as_str()
            ))
            .count(),
        1,
        "recovery must reuse the durable selection instead of creating a second one"
    );
    Ok(())
}

#[tokio::test]
async fn agent_blocks_unstarted_recovery_when_policy_fingerprint_changes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let logical_run_id = "policy-drift-recovery-logical-turn";
    let mut session =
        Session::new("mock-tls-close", "mock-model").with_store(JsonlSessionStore::new(&path)?);
    session.ensure_identity_entry()?;
    let mut durable_user = ModelMessage::user("do not silently alter recovery policy");
    durable_user.id = "policy-drift-user".to_owned();
    session.append_user_message(durable_user.clone())?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-tls-close".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: vec![durable_user],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: Some(ReasoningEffort::Medium),
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let mut original =
        crate::session::ProviderPhysicalAttemptAudit::start(&session, logical_run_id, &frozen)
            .await?;
    let original_id = original
        .physical_attempt_id()
        .expect("durable original physical attempt")
        .to_owned();
    original
        .finish(
            &session,
            ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            None,
        )
        .await?;
    let evidence = ProviderTurnRecoveryEvidenceV1::from_terminal_attempt(
        session
            .provider_physical_attempt_projection()?
            .attempt(&original_id)
            .expect("the original attempt must be projected"),
        ProviderFailureObservationV1::transport_interrupted(
            ProviderWireStateV1::RequestBytesMayHaveBeenSent,
        ),
        &frozen,
    )?;
    let schedule = crate::session::ProviderTurnRecoveryAudit::schedule(
        &session,
        &evidence,
        Default::default(),
        0,
        ProviderTurnRecoveryPolicyV1::default(),
    )
    .await?;
    drop(session);

    let mut resumed = Session::load_from_store(
        "mock-tls-close",
        "mock-model",
        JsonlSessionStore::new(&path)?,
    )?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TlsCloseThenSuccessProvider {
            failures: 0,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    )
    .with_provider_turn_recovery_policy(ProviderTurnRecoveryPolicyV1 {
        max_transport_retries: 1,
        max_partial_output_retries: 0,
        initial_delay_ms: 0,
        max_delay_ms: 0,
        jitter_ratio_millionths: 0,
        max_cumulative_delay_ms: 0,
    })?;
    let mut handler = RecordingEventHandler::default();
    agent
        .run_with_input(
            &mut resumed,
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen)
                .with_logical_run_id(logical_run_id),
            scripted_run_options(1),
            &mut handler,
        )
        .await
        .expect_err("policy drift must block before sending replacement provider bytes");

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let recovery = resumed.provider_turn_recovery_projection()?;
    assert!(matches!(
        recovery.recovery(&schedule.recovery_id),
        Some(state)
            if state.started.is_none()
                && state.exhausted.as_ref().is_some_and(|terminal| {
                    terminal.reason_code == "provider_recovery_policy_changed_re_admit_required"
                        && terminal.terminal_disposition
                            == crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked
                })
    ));
    Ok(())
}

#[tokio::test]
async fn agent_claims_unstarted_durable_recovery_after_session_reload() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let logical_run_id = "recovery-restart-logical-turn";
    let mut session = Session::new("mock-tls-close", "mock-model").with_store(store);
    session.ensure_identity_entry()?;
    let mut durable_user = ModelMessage::user("continue the durable provider turn");
    durable_user.id = "recovery-restart-user".to_owned();
    session.append_user_message(durable_user.clone())?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-tls-close".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: vec![durable_user],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: Some(ReasoningEffort::Medium),
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let mut original =
        crate::session::ProviderPhysicalAttemptAudit::start(&session, logical_run_id, &frozen)
            .await?;
    let original_id = original
        .physical_attempt_id()
        .expect("durable original physical attempt")
        .to_owned();
    original
        .finish(
            &session,
            ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            None,
        )
        .await?;
    let attempts = session.provider_physical_attempt_projection()?;
    let evidence = ProviderTurnRecoveryEvidenceV1::from_terminal_attempt(
        attempts
            .attempt(&original_id)
            .expect("the original attempt must be projected"),
        ProviderFailureObservationV1::transport_interrupted(
            ProviderWireStateV1::RequestBytesMayHaveBeenSent,
        ),
        &frozen,
    )?;
    let schedule = crate::session::ProviderTurnRecoveryAudit::schedule(
        &session,
        &evidence,
        Default::default(),
        500,
        ProviderTurnRecoveryPolicyV1::default(),
    )
    .await?;
    drop(session);

    // The process may have died after schedule append. The new owner waits against the durable
    // absolute deadline and CAS-claims the existing schedule rather than minting a replacement.
    let mut resumed = Session::load_from_store(
        "mock-tls-close",
        "mock-model",
        JsonlSessionStore::new(&path)?,
    )?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TlsCloseThenSuccessProvider {
            failures: 0,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(
            &mut resumed,
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen)
                .with_logical_run_id(logical_run_id),
            scripted_run_options(1),
            &mut handler,
        )
        .await?;

    assert_eq!(output.result.final_text, "done");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let attempts = resumed.provider_physical_attempt_projection()?;
    let chain = attempts.attempts_for_logical_run_id(logical_run_id);
    assert_eq!(chain.len(), 2);
    assert_eq!(chain[0].entry.physical_attempt_id, original_id);
    let recovery = resumed.provider_turn_recovery_projection()?;
    let recovered = recovery
        .recovery(&schedule.recovery_id)
        .expect("recovery schedule must survive reload");
    assert!(recovered.started.is_some());
    assert!(handler.events.iter().any(|event| matches!(
        event,
        RunEvent::ProviderTurnRecovery(view)
            if view.phase == crate::PublicProviderTurnRecoveryPhaseV1::Recovering
    )));
    Ok(())
}

#[tokio::test]
async fn recovery_only_agent_run_never_sends_without_a_durable_schedule() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TlsCloseThenSuccessProvider {
            failures: 0,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session =
        Session::new("mock-tls-close", "mock-model").with_store(JsonlSessionStore::new(&path)?);
    let mut handler = RecordingEventHandler::default();

    let error = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user_with_message_id("resume only", "recovery-only-input")
                .with_logical_run_id("recovery-only-logical-turn")
                .with_durable_provider_recovery_only(),
            scripted_run_options(1),
            &mut handler,
        )
        .await
        .expect_err("an existing participant cannot invent a fresh provider turn after restart");

    assert!(
        error
            .downcast_ref::<crate::ProviderTurnRecoveryTerminalError>()
            .is_some_and(|terminal| terminal.reason_code == "provider_recovery_schedule_missing")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        session
            .provider_physical_attempt_projection()?
            .attempts_for_logical_run_id("recovery-only-logical-turn")
            .is_empty()
    );
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert!(records.iter().any(|record| matches!(
        record,
        SessionStreamRecord::Stored(event)
            if event.event_type == DurableEventType::RunFinalized.as_str()
                && event.payload.get("run_status").and_then(Value::as_str) == Some("paused")
    )));
    Ok(())
}

#[tokio::test]
async fn agent_reload_blocks_started_recovery_without_reissuing_provider_bytes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let logical_run_id = "recovery-started-restart-logical-turn";
    let mut session = Session::new("mock-tls-close", "mock-model").with_store(store);
    session.ensure_identity_entry()?;
    let mut durable_user = ModelMessage::user("do not reissue an uncertain recovery");
    durable_user.id = "recovery-started-restart-user".to_owned();
    session.append_user_message(durable_user.clone())?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-tls-close".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: vec![durable_user],
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            reasoning_effort: Some(ReasoningEffort::Medium),
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let mut original =
        crate::session::ProviderPhysicalAttemptAudit::start(&session, logical_run_id, &frozen)
            .await?;
    let original_id = original
        .physical_attempt_id()
        .expect("durable original physical attempt")
        .to_owned();
    original
        .finish(
            &session,
            ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            None,
        )
        .await?;
    let attempts = session.provider_physical_attempt_projection()?;
    let evidence = ProviderTurnRecoveryEvidenceV1::from_terminal_attempt(
        attempts
            .attempt(&original_id)
            .expect("the original attempt must be projected"),
        ProviderFailureObservationV1::transport_interrupted(
            ProviderWireStateV1::RequestBytesMayHaveBeenSent,
        ),
        &frozen,
    )?;
    let schedule = crate::session::ProviderTurnRecoveryAudit::schedule(
        &session,
        &evidence,
        Default::default(),
        0,
        ProviderTurnRecoveryPolicyV1::default(),
    )
    .await?;
    let started = crate::session::ProviderTurnRecoveryAudit::start(&session, &schedule).await?;
    let _abandoned_after_send_barrier =
        crate::session::ProviderPhysicalAttemptAudit::start_recovery(
            &session,
            logical_run_id,
            &frozen,
            &schedule,
            &started.physical_attempt_id,
        )
        .await?;
    drop(session);

    let mut resumed = Session::load_from_store(
        "mock-tls-close",
        "mock-model",
        JsonlSessionStore::new(&path)?,
    )?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        TlsCloseThenSuccessProvider {
            failures: 0,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut handler = RecordingEventHandler::default();
    agent
        .run_with_input(
            &mut resumed,
            AgentRunInput::without_persisted_user_message(Vec::new())
                .with_initial_frozen_provider_request(frozen)
                .with_logical_run_id(logical_run_id),
            scripted_run_options(1),
            &mut handler,
        )
        .await
        .expect_err("a started recovery must not resend provider bytes after process loss");

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let attempts = resumed.provider_physical_attempt_projection()?;
    let chain = attempts.attempts_for_logical_run_id(logical_run_id);
    assert_eq!(chain.len(), 2);
    assert!(matches!(
        chain[1].terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::Interrupted,
            ..
        })
    ));
    let recovery = resumed.provider_turn_recovery_projection()?;
    assert!(matches!(
        recovery.terminal_for_logical_run_id(logical_run_id),
        Some(terminal)
            if terminal.reason_code == "recovery_started_without_safe_completion"
                && terminal.terminal_disposition
                    == crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked
    ));
    Ok(())
}

#[tokio::test]
async fn agent_bounds_confirmed_pre_dispatch_connect_retries() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        ConnectRetryProvider {
            connect_failures: usize::MAX,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-connect-retry", "mock-model").with_store(store);
    let mut handler = RecordingEventHandler::default();

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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("the third confirmed connect failure should exhaust the retry budget");

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let projection = session.provider_physical_attempt_projection()?;
    let records = JsonlSessionStore::read_event_records(&path)?;
    let logical_run_id = records
        .iter()
        .find_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptStarted.as_str() =>
            {
                serde_json::from_value::<ProviderPhysicalAttemptStartedEntry>(event.payload.clone())
                    .ok()
                    .map(|entry| entry.logical_run_id)
            }
            _ => None,
        })
        .expect("provider attempt should be durable");
    let attempts = projection.attempts_for_logical_run_id(&logical_run_id);
    assert_eq!(attempts.len(), 3);
    assert!(attempts.iter().all(|attempt| matches!(
        attempt.terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
            rejection: Some(ProviderRequestRejection::ConnectFailedBeforeDispatch),
            ..
        })
    )));
    Ok(())
}

#[tokio::test]
async fn bound_initial_physical_attempt_keeps_identity_then_uses_durable_recovery() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        ConnectRetryProvider {
            connect_failures: usize::MAX,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-connect-retry", "mock-model").with_store(store);
    let mut handler = RecordingEventHandler::default();
    let physical_attempt_id = "provider-attempt-bound-user-input";

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("hi")
                .with_initial_provider_physical_attempt_id(physical_attempt_id.to_owned()),
            AgentRunOptions {
                workspace_root: std::env::temp_dir(),
                max_turns: Some(1),
                tool_timeout_secs: 5,
                reasoning_effort: Some(ReasoningEffort::Medium),
                traffic_partition_key: None,
                interaction_mode: InteractionMode::Interactive,
                permission_config: PermissionConfig::default(),
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("bounded provider-turn recovery should pause after its durable budget is used");

    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let projection = session.provider_physical_attempt_projection()?;
    let attempt = projection
        .attempt(physical_attempt_id)
        .expect("the preallocated physical identity should cross the send barrier");
    assert!(matches!(
        attempt.terminal.as_ref(),
        Some(ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::ConfirmedNoModelConsumption,
            rejection: Some(ProviderRequestRejection::ConnectFailedBeforeDispatch),
            ..
        })
    ));
    let recovery = session.provider_turn_recovery_projection()?;
    let logical_run_id = attempt.entry.logical_run_id.clone();
    assert_eq!(
        recovery
            .recoveries_for_logical_run_id(&logical_run_id)
            .len(),
        2
    );
    assert!(matches!(
        recovery.terminal_for_logical_run_id(&logical_run_id),
        Some(terminal) if terminal.terminal_disposition
            == crate::ProviderTurnRecoveryTerminalDispositionV1::Paused
    ));
    Ok(())
}

#[tokio::test]
async fn agent_cancellation_during_connect_backoff_starts_no_new_attempt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        ConnectRetryProvider {
            connect_failures: usize::MAX,
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-connect-retry", "mock-model").with_store(store);
    let mut handler = RecordingEventHandler::default();
    let cancellation_owner = RunCancellationOwner::new();
    let input = AgentRunInput::user("hi").with_cancellation(cancellation_owner.handle());
    let run = agent.run_with_input(
        &mut session,
        input,
        AgentRunOptions {
            workspace_root: std::env::temp_dir(),
            max_turns: Some(1),
            tool_timeout_secs: 5,
            reasoning_effort: Some(ReasoningEffort::Medium),
            traffic_partition_key: None,
            interaction_mode: InteractionMode::Interactive,
            permission_config: PermissionConfig::default(),
            permission_mode_override: None,
            permission_context: crate::PermissionEvaluationContext::default(),
            memory_config: MemoryConfig::with_enabled(false),
            compaction_config: CompactionConfig::default(),
            tool_authority: None,
        },
        &mut handler,
    );
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => panic!("run ended before entering connect backoff: {result:#?}"),
        () = async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        } => {}
    }
    assert!(cancellation_owner.request_cancel());

    run.await
        .expect_err("cancellation should stop before another physical attempt starts");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let records = JsonlSessionStore::read_event_records(&path)?;
    assert_eq!(
        records
            .iter()
            .filter(|record| matches!(
                record,
                SessionStreamRecord::Stored(event)
                    if event.event_type
                        == DurableEventType::ProviderPhysicalAttemptStarted.as_str()
            ))
            .count(),
        1
    );
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
            rejection: Some(ProviderRequestRejection::ConnectFailedBeforeDispatch),
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn agent_never_retries_an_unclassified_pre_stream_transport_error() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let calls = Arc::new(AtomicUsize::new(0));
    let agent = Agent::new(
        UnclassifiedPreStreamErrorProvider {
            calls: Arc::clone(&calls),
        },
        ToolRegistry::new(),
    );
    let mut session =
        Session::new("mock-unclassified-pre-stream-error", "mock-model").with_store(store);
    let mut handler = RecordingEventHandler::default();

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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("an uncertain transport error must surface without retry");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let records = JsonlSessionStore::read_event_records(&path)?;
    let terminals = records
        .iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event)
                if event.event_type
                    == DurableEventType::ProviderPhysicalAttemptTerminal.as_str() =>
            {
                serde_json::from_value::<ProviderPhysicalAttemptTerminalEntry>(
                    event.payload.clone(),
                )
                .ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        terminals.as_slice(),
        [ProviderPhysicalAttemptTerminalEntry {
            outcome: ProviderPhysicalAttemptOutcome::TransportOutcomeUncertain,
            rejection: None,
            ..
        }]
    ));
    Ok(())
}

#[tokio::test]
async fn agent_blocks_unclassified_provider_stream_errors_with_safe_recovery_context() -> Result<()>
{
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("unclassified provider error should stop with an actionable recovery terminal");

    assert!(error.to_string().contains("provider-turn recovery Blocked"));
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
    let finalized = finalized.expect("run finalized event should be present for provider recovery");
    assert_eq!(
        finalized.payload.get("run_status").and_then(Value::as_str),
        Some("paused")
    );
    assert_eq!(
        finalized
            .payload
            .get("terminal_reason")
            .and_then(Value::as_str),
        Some("provider_request_requires_attention")
    );
    assert!(finalized.payload.get("error").is_some_and(Value::is_null));
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
    let recovery = session.provider_turn_recovery_projection()?;
    let logical_run_id = recovery
        .terminal_for_logical_run_id(
            &session
                .provider_physical_attempt_projection()?
                .attempts()
                .first()
                .expect("provider attempt should be persisted")
                .entry
                .logical_run_id,
        )
        .expect("provider recovery terminal should be durable");
    assert_eq!(
        logical_run_id.terminal_disposition,
        crate::ProviderTurnRecoveryTerminalDispositionV1::Blocked
    );
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

#[tokio::test]
async fn typed_provider_protocol_violation_is_durable_post_output_evidence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("session.jsonl");
    let store = JsonlSessionStore::new(&path)?;
    let agent = Agent::new(TypedProtocolViolationProvider, ToolRegistry::new());
    let mut session = Session::new("mock-typed-protocol-violation", "mock-model").with_store(store);
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
            },
            &mut handler,
        )
        .await
        .expect_err("typed protocol violation must fail the current physical attempt");
    assert_eq!(
        error.downcast_ref::<crate::ProviderProtocolViolation>(),
        Some(&crate::ProviderProtocolViolation::UnstructuredToolInvocation)
    );

    let projection = session.provider_physical_attempt_projection()?;
    let terminal = projection
        .attempts()
        .into_iter()
        .next()
        .and_then(|attempt| attempt.terminal.as_ref())
        .expect("physical attempt terminal");
    assert_eq!(
        terminal.outcome,
        ProviderPhysicalAttemptOutcome::ProtocolRejectedAfterOutput
    );
    assert!(terminal.durable_side_effect_event_ids.is_empty());
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        _args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, path_tool_subject(args)?, None)
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

    fn permission_plan(
        &self,
        _ctx: &ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        declared_test_permission_plan(self, args, path_tool_subject(args)?, None)
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
fn agent_into_parts_preserves_provider_and_tool_registry() {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = Agent::new(MockProvider, tools);

    let (provider, tools) = agent.into_parts();

    assert_eq!(provider.name(), "mock");
    assert!(tools.spec_for("echo").is_some());
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
    assert_eq!(
        context["command_sha256"],
        super::stable_text_hash(
            json!({
                "command": format!("  echo   {}  ", "x".repeat(220)),
                "path": "notes/file.txt",
                "pattern": "needle",
            })["command"]
                .as_str()
                .expect("command")
        )
    );
    assert_eq!(
        context["path_sha256"],
        super::stable_text_hash("notes/file.txt")
    );
    assert_eq!(context["pattern_sha256"], super::stable_text_hash("needle"));
    assert_eq!(context["subjects"].as_array().map(Vec::len), Some(6));
    assert_eq!(
        context["subjects"][0],
        format!(
            "workspace:path:{}",
            super::stable_text_hash("notes/file.txt")
        )
    );
    assert!(
        context["summary"]
            .as_str()
            .is_some_and(|value| value.contains("command_sha256="))
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
        null_details.metadata.details["call"]["path_sha256"],
        super::stable_text_hash("notes/file.txt")
    );

    let mut object_details = ToolResult::ok("call-ctx", "bash", "ok", ToolResultMeta::default());
    object_details.metadata.details = json!({ "existing": true });
    super::attach_tool_call_context(&mut object_details, &call, &subjects);
    assert_eq!(object_details.metadata.details["existing"], true);
    assert_eq!(
        object_details.metadata.details["call"]["pattern_sha256"],
        super::stable_text_hash("needle")
    );

    let mut string_details = ToolResult::ok("call-ctx", "bash", "ok", ToolResultMeta::default());
    string_details.metadata.details = Value::String("previous".to_owned());
    super::attach_tool_call_context(&mut string_details, &call, &subjects);
    assert_eq!(string_details.metadata.details["tool"], "previous");
    assert_eq!(
        string_details.metadata.details["call"]["path_sha256"],
        super::stable_text_hash("notes/file.txt")
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
    let mut decision = PermissionDecision::new(
        ApprovalMode::Ask,
        "write_file",
        ToolAccess::Write,
        subjects.clone(),
        true,
    );
    decision.confirmation = Some(crate::PermissionConfirmation::TypePhrase {
        phrase: "approval-secret-phrase".to_owned(),
    });
    decision.command_permission_matches = vec![crate::CommandPermissionMatch {
        group: crate::CommandPermissionGroup::Ask,
        pattern: "curl * approval-secret-pattern".to_owned(),
        command: "curl https://example.test?token=approval-secret-command".to_owned(),
    }];
    decision.reasons = vec![crate::PermissionDecisionReason {
        source: crate::PermissionDecisionSource::HardSafety,
        code: "sensitive_test".to_owned(),
        detail: format!(
            "Review https://example.test?token=approval-secret-reason {}",
            "x".repeat(2_048)
        ),
    }];
    let plan = crate::ToolPermissionPlanV2::bind(
        &call.name,
        &serde_json::from_str(&call.args_json)?,
        std::path::Path::new("."),
        crate::ToolPermissionPlanDraft {
            access: ToolAccess::Write,
            operation: crate::ToolOperation::EditFile,
            effects: BTreeSet::from([crate::ToolPermissionEffect::FileWrite]),
            subjects: subjects.clone(),
            analysis: crate::ToolAnalysisStatus::Complete,
            containment: crate::ExecutionContainmentRequest::default(),
            semantic_scope: Some(crate::ToolSemanticScope::new("workspace_edit", 1)),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::new(),
            safe_summary: crate::ToolPermissionSummary {
                title: "Edit file".to_owned(),
                detail: "test fixture".to_owned(),
                ..crate::ToolPermissionSummary::default()
            },
        },
    )?;
    let identity = crate::ApprovalRequestIdentityV2 {
        session_id: "deepseek".to_owned(),
        run_id: "test-run".to_owned(),
        call_id: call.id.clone(),
        approval_request_id: "test-approval".to_owned(),
        plan_hash: plan.plan_hash.clone(),
        policy_version: "sha256:test-policy".to_owned(),
        execution_binding_hash: plan.plan_hash.clone(),
        expires_at_ms: 1_000,
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
        &identity,
        &plan,
        ToolApprovalAuditAction::Requested,
        None,
        None,
        None,
        Some("a".repeat(64)),
    )?;
    super::append_tool_execution_audit(
        &mut session,
        &call,
        &subjects,
        ToolExecutionStatus::Started,
        None,
        None,
    )?;
    let mut result = ToolResult::error(
        "call-1",
        "write_file",
        ToolErrorKind::PermissionDenied,
        "denied token=durable-secret-value",
    )
    .with_error_details(
        true,
        json!({
            "reason": "policy",
            "secret": "durable-secret-value",
            "path": "/Users/private/workspace/file.txt",
            "payload": "x".repeat(128_000),
        }),
    );
    result.metadata.changed_files = vec!["/Users/private/workspace/file.txt".to_owned()];
    result.metadata.details = json!({
        "custom": "durable-secret-value",
        "command": "cat /Users/private/workspace/file.txt",
    });
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
                    && approval.preview_hash.as_deref() == Some("a".repeat(64).as_str())
                    && approval.external_directory_required
        )
    }));
    let execution = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.status == ToolExecutionStatus::Failed =>
            {
                Some(execution)
            }
            _ => None,
        })
        .expect("failed tool execution audit");
    let execution_json = serde_json::to_string(execution)?;
    assert!(execution_json.len() <= crate::MAX_DURABLE_TOOL_EXECUTION_BYTES);
    assert!(!execution_json.contains("durable-secret-value"));
    assert!(!execution_json.contains("/Users/private"));
    assert_eq!(execution.metadata.changed_files.len(), 1);
    assert!(execution.metadata.changed_files[0].starts_with("sha256:"));
    assert_eq!(
        execution.error.as_ref().expect("error").details["redacted"],
        true
    );
    let approval = session
        .entries()
        .iter()
        .find_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval)) => Some(approval),
            _ => None,
        })
        .expect("tool approval audit");
    let approval_json = serde_json::to_string(approval)?;
    assert!(approval_json.len() <= crate::MAX_DURABLE_TOOL_CONTROL_BYTES);
    assert!(!approval_json.contains("approval-secret"));
    assert!(!approval_json.contains("curl"));
    assert!(!approval_json.contains("https://"));
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
async fn agent_surfaces_invalid_permission_plan_with_usage_snapshot() -> Result<()> {
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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
                    permission_mode_override: None,
                    permission_context: crate::PermissionEvaluationContext::default(),
                    memory_config: MemoryConfig::with_enabled(false),
                    compaction_config: CompactionConfig::default(),
                    tool_authority: None,
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
                permission_mode_override: None,
                permission_context: crate::PermissionEvaluationContext::default(),
                memory_config: MemoryConfig::with_enabled(false),
                compaction_config: CompactionConfig::default(),
                tool_authority: None,
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

#[test]
fn assistant_batch_settlement_keeps_provider_previews_within_budget() -> Result<()> {
    // RFC-0062 11.2 integration: four parallel 32 KiB results settle through the batch
    // allocator; the durable V3 records' provider-visible previews must total <= 64 KiB, every
    // result keeps its deterministic minimum preview, and the declaration order is preserved.
    let mut session = Session::new("test", "model");
    let mut handler = RecordingEventHandler::default();
    let mut outcome = AgentRunOutcome::default();
    let batch = (0..4)
        .map(|index| {
            let call = crate::ToolCall {
                id: format!("call-{index}"),
                name: "shell".to_owned(),
                args_json: "{}".to_owned(),
            };
            let result = ToolResult::ok(
                format!("call-{index}"),
                "shell",
                "x".repeat(32 * 1024),
                ToolResultMeta::default(),
            );
            (call, result)
        })
        .collect::<Vec<_>>();
    crate::agent::tool_results::emit_tool_result_batch(
        &mut session,
        &mut handler,
        &mut outcome,
        batch,
    )?;

    let mut preview_total = 0usize;
    let mut order = Vec::new();
    for entry in session.entries() {
        if let SessionLogEntry::ToolResultV3(result) = entry {
            preview_total = preview_total.saturating_add(result.initial_model_view.preview.len());
            order.push(result.call_id.clone());
            assert!(result.initial_model_view.preview.len() >= 512);
        }
    }
    assert!(preview_total <= 64 * 1024);
    assert_eq!(order, vec!["call-0", "call-1", "call-2", "call-3"]);
    Ok(())
}

/// Scripted provider: each turn pops the next declared (call_id, tool name, args) batch; once the
/// script is exhausted the provider answers with a plain final text turn.
struct ScriptedTurnToolProvider {
    turns: Mutex<std::collections::VecDeque<Vec<(String, String, String)>>>,
}

struct CacheConformanceProvider {
    calls: AtomicUsize,
    captured: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl ScriptedTurnToolProvider {
    fn new(turns: Vec<Vec<(String, String, String)>>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
        }
    }
}

#[async_trait]
impl Provider for ScriptedTurnToolProvider {
    fn name(&self) -> &str {
        "mock-scripted-tool"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        WriteMockProvider.capabilities()
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        let turn = self
            .turns
            .lock()
            .expect("scripted turns lock should not be poisoned")
            .pop_front();
        let Some(calls) = turn else {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::TextDelta("ordinary answer".to_owned())),
                Ok(ProviderChunk::Done),
            ])));
        };
        let mut chunks = Vec::new();
        for (id, name, args) in calls {
            chunks.push(Ok(ProviderChunk::ToolCallStart {
                id: id.clone(),
                name: name.clone(),
            }));
            chunks.push(Ok(ProviderChunk::ToolCallArgsDelta {
                id: id.clone(),
                delta: args.clone(),
            }));
            chunks.push(Ok(ProviderChunk::ToolCallComplete(ToolCall {
                id,
                name,
                args_json: args,
            })));
        }
        chunks.push(Ok(ProviderChunk::Done));
        Ok(Box::pin(stream::iter(chunks)))
    }
}

#[async_trait]
impl Provider for CacheConformanceProvider {
    fn name(&self) -> &str {
        "mock-cache-conformance"
    }

    fn capabilities(&self) -> ProviderCapabilities {
        MockProvider.capabilities()
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<ProviderChunk>> + Send>>> {
        self.captured
            .lock()
            .expect("cache conformance capture lock should not be poisoned")
            .push(request);
        let ordinal = self.calls.fetch_add(1, Ordering::SeqCst);
        if ordinal == 0 {
            return Ok(Box::pin(stream::iter(vec![
                Ok(ProviderChunk::ToolCallStart {
                    id: "cache-tool-call".to_owned(),
                    name: "echo".to_owned(),
                }),
                Ok(ProviderChunk::ToolCallArgsDelta {
                    id: "cache-tool-call".to_owned(),
                    delta: r#"{"value":"cache-stable tool result"}"#.to_owned(),
                }),
                Ok(ProviderChunk::ToolCallComplete(ToolCall {
                    id: "cache-tool-call".to_owned(),
                    name: "echo".to_owned(),
                    args_json: r#"{"value":"cache-stable tool result"}"#.to_owned(),
                })),
                Ok(ProviderChunk::ContinuationState(
                    crate::ProviderContinuationState {
                        provider_name: "mock-cache-conformance".to_owned(),
                        state_kind: "cache-replay".to_owned(),
                        message_id: None,
                        opaque_blob: serde_json::json!({"stable": "durable-state"}),
                    },
                )),
                Ok(ProviderChunk::Done),
            ])));
        }
        Ok(Box::pin(stream::iter(vec![
            Ok(ProviderChunk::TextDelta(format!(
                "cache conformance answer {ordinal}"
            ))),
            Ok(ProviderChunk::Done),
        ])))
    }
}

fn cache_conformance_context(id: &str, body: &str) -> RuntimeContextCandidates {
    let mut runtime_context = RuntimeContextCandidates::new();
    runtime_context.items.push(ContextItem {
        id: id.to_owned(),
        source: ContextSource::RepositoryFile,
        source_event_id: None,
        trust_level: ContextTrustLevel::UntrustedRepositoryData,
        sensitivity: ContextSensitivity::Repository,
        egress_decision: None,
        repo_revision: Some(format!("revision-{id}")),
        token_cost: crate::estimate_context_token_cost(body),
        score: Some(100.0),
        score_breakdown: Vec::new(),
        inclusion_reason: ContextInclusionReason::RetrievalHit,
        body_ref: ContextBodyRef::inline(body),
    });
    runtime_context
        .snippets
        .insert(id.to_owned(), body.to_owned());
    runtime_context
}

fn assert_cache_stable_tail_extension(previous: &CompletionRequest, next: &CompletionRequest) {
    assert_eq!(previous.provider_name, next.provider_name);
    assert_eq!(previous.model_name, next.model_name);
    assert_eq!(
        serde_json::to_value(&previous.tools).expect("serialize previous tools"),
        serde_json::to_value(&next.tools).expect("serialize next tools")
    );
    assert!(next.messages.len() > previous.messages.len());
    assert_eq!(
        serde_json::to_value(&previous.messages).expect("serialize previous messages"),
        serde_json::to_value(&next.messages[..previous.messages.len()])
            .expect("serialize next message prefix")
    );
}

fn scripted_run_options(max_turns: usize) -> AgentRunOptions {
    AgentRunOptions {
        workspace_root: std::env::temp_dir(),
        max_turns: Some(max_turns),
        tool_timeout_secs: 5,
        reasoning_effort: Some(ReasoningEffort::Medium),
        traffic_partition_key: None,
        interaction_mode: InteractionMode::Interactive,
        permission_config: PermissionConfig::default(),
        permission_context: crate::PermissionEvaluationContext::default(),
        permission_mode_override: None,
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
    }
}

fn conversation_run_purpose(
    logical_run_id: &str,
    source_turn: ConversationTurnRef,
    routing_policy: TaskRoutingPolicy,
    route_capability: AutomaticRouteCapability,
    plan_review: Option<PlanReviewHandoffBinding>,
    task_handoff: Option<TaskPlanningHandoffBinding>,
) -> AgentRunPurpose {
    AgentRunPurpose::Conversation(Box::new(ConversationPurposeContext {
        root_run_id: logical_run_id.to_owned(),
        source_turn,
        routing_policy,
        route_capability,
        writable_memory_routing: false,
        task_continuation: None,
        plan_review,
        task_handoff,
    }))
}

fn settled_tool_results(session: &Session) -> Vec<(String, String)> {
    session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::ToolResultV3(result) => Some((
                result.call_id.clone(),
                result.initial_model_view.preview.clone(),
            )),
            _ => None,
        })
        .collect()
}

fn tool_result_event_count(handler: &RecordingEventHandler, call_id: &str) -> usize {
    handler
        .events
        .iter()
        .filter(|event| matches!(event, RunEvent::ToolResult(result) if result.call_id == call_id))
        .count()
}

fn assert_single_settled_result(session: &Session, call_id: &str, preview_contains: &str) {
    let settled = settled_tool_results(session);
    let matching = settled
        .iter()
        .filter(|(id, _)| id == call_id)
        .collect::<Vec<_>>();
    assert_eq!(
        matching.len(),
        1,
        "call {call_id} must settle exactly once, got {matching:?}"
    );
    assert!(
        matching[0].1.contains(preview_contains),
        "preview {:?} must contain {preview_contains:?}",
        matching[0].1
    );
}

#[tokio::test]
async fn production_agent_requests_preserve_cache_prefix_across_tool_turn_user_turn_and_resume()
-> Result<()> {
    let temp = tempfile::tempdir()?;
    let store_path = temp.path().join("cache-conformance.jsonl");
    let store = JsonlSessionStore::new(&store_path)?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut registry = ToolRegistry::new();
    let cache_tool = Arc::new(ScheduledReadTool {
        name: "echo".to_owned(),
        delay: Duration::ZERO,
        parallel: true,
        mutation_tracking: ToolMutationTracking::None,
        fail: false,
        probe: Arc::new(ToolSchedulerProbe::default()),
    });
    let reconstruction_tools = vec![cache_tool.spec()];
    registry.register(cache_tool);
    let agent = Agent::new(
        CacheConformanceProvider {
            calls: AtomicUsize::new(0),
            captured: Arc::clone(&captured),
        },
        registry,
    );
    let mut session =
        Session::new("mock-cache-conformance", "mock-model").with_store(store.clone());
    session.ensure_identity_entry()?;
    let mut handler = RecordingEventHandler::default();

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect the first state")
                .with_logical_run_id("cache-run-1")
                .with_runtime_context(cache_conformance_context(
                    "context-a",
                    "stable repository context A",
                )),
            scripted_run_options(4),
            &mut handler,
        )
        .await?;
    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect the second state")
                .with_logical_run_id("cache-run-2")
                .with_runtime_context(cache_conformance_context(
                    "context-b",
                    "new repository context B",
                )),
            scripted_run_options(2),
            &mut handler,
        )
        .await?;

    drop(session);
    let mut resumed = Session::load_from_store(
        "mock-cache-conformance",
        "mock-model",
        JsonlSessionStore::new(&store_path)?,
    )?;
    agent
        .run_with_input(
            &mut resumed,
            AgentRunInput::user("inspect after resume")
                .with_logical_run_id("cache-run-3")
                .with_runtime_context(cache_conformance_context(
                    "context-c",
                    "resumed repository context C",
                )),
            scripted_run_options(2),
            &mut handler,
        )
        .await?;
    let requests = captured
        .lock()
        .expect("cache conformance capture lock should not be poisoned")
        .clone();
    assert_eq!(requests.len(), 4);
    for pair in requests.windows(2) {
        assert_cache_stable_tail_extension(&pair[0], &pair[1]);
    }

    let projection = ProviderPhysicalAttemptProjection::from_records(
        &JsonlSessionStore::read_event_records(&store_path)?,
    )?;
    // A tool follow-up is a distinct logical provider turn. The durable attempts still preserve
    // one continuous request frontier, so audit the session-wide start order against the
    // provider capture rather than assuming every turn owns the caller's root logical id.
    let attempts = projection.attempts();
    assert_eq!(attempts.len(), 4);
    for (index, attempt) in attempts.iter().enumerate() {
        let envelope = attempt
            .entry
            .request_envelope
            .as_ref()
            .expect("production attempt must bind a request envelope");
        assert!(envelope.source_frontier.is_some());
        let rebuilt =
            reconstruct_cache_conformance_request(&store_path, envelope, &reconstruction_tools)?;
        assert_eq!(
            serde_json::to_value(&rebuilt)?,
            serde_json::to_value(&requests[index])?
        );
        envelope.verify_reconstructed_request_at_frontier(&store_path, &rebuilt)?;
        if index == 0 {
            let mut tampered = envelope.clone();
            let frontier = tampered
                .source_frontier
                .as_mut()
                .expect("the production attempt must bind a durable source frontier");
            frontier.durable_end_offset = frontier.durable_end_offset.saturating_sub(1);
            assert!(
                tampered
                    .verify_reconstructed_request_at_frontier(&store_path, &rebuilt)
                    .is_err(),
                "a byte offset inside the terminal JSONL record must fail closed"
            );
        }
        if index > 0 {
            let mutation = &attempt
                .entry
                .cache_layout_proof
                .as_ref()
                .expect("cache layout proof")
                .mutation_from_previous;
            assert_eq!(
                mutation.kind,
                crate::CacheLayoutMutationKind::ConversationTailAppended
            );
            assert!(mutation.local_stable_prefix_preserved);
        }
    }
    activate_cache_conformance_compaction(&resumed, &store_path)?;
    drop(resumed);
    let mut compacted = Session::load_from_store(
        "mock-cache-conformance",
        "mock-model",
        JsonlSessionStore::new(&store_path)?,
    )?;
    agent
        .run_with_input(
            &mut compacted,
            AgentRunInput::user("inspect after compaction")
                .with_logical_run_id("cache-run-4")
                .with_runtime_context(cache_conformance_context(
                    "context-d",
                    "rebuilt repository context D",
                )),
            scripted_run_options(2),
            &mut handler,
        )
        .await?;
    let requests = captured
        .lock()
        .expect("cache conformance capture lock should not be poisoned");
    assert_eq!(requests.len(), 5);
    assert_ne!(
        serde_json::to_value(&requests[3].messages)?,
        serde_json::to_value(
            &requests[4].messages[..requests[3].messages.len().min(requests[4].messages.len())]
        )?,
        "the first request in a compacted epoch must not masquerade as an append-only extension"
    );
    let projection = ProviderPhysicalAttemptProjection::from_records(
        &JsonlSessionStore::read_event_records(&store_path)?,
    )?;
    let post_attempt = projection
        .attempts_for_logical_run_id("cache-run-4")
        .into_iter()
        .next()
        .expect("post-compaction run must have one provider attempt");
    let envelope = post_attempt
        .entry
        .request_envelope
        .as_ref()
        .expect("post-compaction attempt must bind a request envelope");
    let rebuilt =
        reconstruct_cache_conformance_request(&store_path, envelope, &reconstruction_tools)?;
    assert_eq!(
        serde_json::to_value(&rebuilt)?,
        serde_json::to_value(&requests[4])?
    );
    envelope.verify_reconstructed_request_at_frontier(&store_path, &rebuilt)?;
    let mutation = &post_attempt
        .entry
        .cache_layout_proof
        .as_ref()
        .expect("post-compaction attempt must bind cache layout proof")
        .mutation_from_previous;
    assert_eq!(
        mutation.kind,
        crate::CacheLayoutMutationKind::ConversationHistoryRewritten
    );
    assert!(!mutation.local_stable_prefix_preserved);
    Ok(())
}

fn reconstruct_cache_conformance_request(
    store_path: &std::path::Path,
    envelope: &crate::ProviderRequestEnvelopeV1,
    reconstruction_tools: &[crate::ToolSpec],
) -> Result<CompletionRequest> {
    let frontier = envelope
        .source_frontier
        .as_ref()
        .expect("production cache envelope must have a durable frontier");
    let reconstructed = Session::reconstruct_from_provider_frontier(
        envelope.provider_name.clone(),
        envelope.model_name.clone(),
        store_path,
        frontier,
    )?;
    reconstructed.reconstruct_provider_request(
        &std::env::temp_dir(),
        &MemoryConfig::with_enabled(false),
        reconstruction_tools.to_vec(),
        None,
        Some(ReasoningEffort::Medium),
        None,
        None,
        &[],
    )
}

fn activate_cache_conformance_compaction(
    session: &Session,
    store_path: &std::path::Path,
) -> Result<()> {
    let store = JsonlSessionStore::new(store_path)?;
    let records = store.read_event_records_writer()?;
    let plan = crate::CompactionFoldPlan::from_records_after_adaptive_tail(
        &records,
        crate::AdaptiveTailPolicyV3 {
            tail_target_min_tokens: 1,
            tail_target_max_tokens: 1,
            ..crate::AdaptiveTailPolicyV3::default()
        },
        u64::MAX / 4,
        None,
    )?;
    let source_event_id = plan
        .folded_event_ids
        .first()
        .cloned()
        .expect("cache conformance fixture must have foldable history");
    let preflight =
        store.prepare_portable_semantic_compaction(crate::PortableSemanticCompactionRequest {
            attempt_id: "cache-conformance-compaction-attempt".to_owned(),
            compaction_id: "cache-conformance-compaction".to_owned(),
            initiation: crate::CompactionInitiation::Manual,
            base_projection_revision: "cache-conformance-compaction-r1".to_owned(),
            branch_id: None,
            valid_for_snapshot: "cache-conformance-snapshot-r1".to_owned(),
            objective: Some("Preserve cache conformance across epoch rotation".to_owned()),
            language: "en".to_owned(),
            plan,
            model_output: crate::ContinuationModelOutputV1 {
                in_progress: vec![crate::ContinuationModelOutputItemV1 {
                    text: "Continue validating cache behavior after epoch rotation.".to_owned(),
                    source_event_ids: vec![source_event_id],
                    priority: crate::ContinuationItemPriority::Critical,
                }],
                pending_actions: Vec::new(),
                provider_continuity: Vec::new(),
                model_notes: Vec::new(),
            },
            tool_output_projection_policy: crate::ToolOutputProjectionPolicy::default(),
            started_at_unix_ms: 10,
            completed_at_unix_ms: 11,
        })?;
    let frozen = FrozenProviderRequestMaterial::freeze(
        session.session_scope_id(),
        CompletionRequest {
            provider_name: "mock-cache-conformance".to_owned(),
            model_name: "mock-model".to_owned(),
            messages: preflight.candidate_messages().to_vec(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: Some(128),
            reasoning_effort: Some(ReasoningEffort::Medium),
            previous_response_handle: None,
            continuation_states: Vec::new(),
            traffic_partition_key: None,
            background: false,
            store: false,
            deterministic_materialization: true,
            hosted_tools: Vec::new(),
        },
    )?;
    let profile = |id: &str| crate::VersionedProfileIdentity::from_content(id, 1, id.as_bytes());
    let binding = crate::TokenMeasurementBinding {
        schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        provider_name: "mock-cache-conformance".to_owned(),
        model_name: "mock-model".to_owned(),
        wire_profile: profile("cache-conformance-wire"),
        token_measurement_profile: profile("cache-conformance-tokenizer"),
        hosted_parity_profile: Some(profile("cache-conformance-hosted-parity")),
    };
    let proof = crate::RequestFitProof {
        schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
        input: crate::InputTokenEvidence::Exact {
            tokens: 10,
            material_fingerprint: frozen.fingerprint().to_owned(),
            measurement_scope: crate::TokenMeasurementScope::RenderedTargetInput,
            binding: binding.clone(),
            provider_model_snapshot: None,
            provider_system_fingerprint: None,
        },
        budget: crate::EffectiveTokenBudget {
            schema_version: crate::COMPACTION_TOKEN_PROOF_SCHEMA_VERSION,
            budget_profile: profile("cache-conformance-budget"),
            context_window_tokens: 128_000,
            requested_output_tokens: 128,
            safety_buffer_tokens: 1_024,
        },
    };
    let before_input = crate::InputTokenEvidence::Exact {
        tokens: 8_000,
        material_fingerprint: frozen.fingerprint().to_owned(),
        measurement_scope: crate::TokenMeasurementScope::RenderedTargetInput,
        binding: binding.clone(),
        provider_model_snapshot: None,
        provider_system_fingerprint: None,
    };
    let target = crate::PortableTargetRequestMaterial::new(frozen.clone(), binding, proof)
        .with_portable_economics(&frozen, before_input)?;
    store.execute_portable_semantic_compaction(preflight, target)?;
    Ok(())
}

#[tokio::test]
async fn parallel_read_only_tool_lane_is_bounded_and_commits_in_declaration_order() -> Result<()> {
    let probe = Arc::new(ToolSchedulerProbe::default());
    let mut registry = ToolRegistry::new();
    let calls = (0..6)
        .map(|index| {
            let name = format!("scheduled_read_{index}");
            registry.register(Arc::new(ScheduledReadTool {
                name: name.clone(),
                delay: Duration::from_millis(if index == 0 { 80 } else { 20 }),
                parallel: true,
                mutation_tracking: ToolMutationTracking::None,
                fail: false,
                probe: Arc::clone(&probe),
            }));
            (format!("call-{index}"), name, "{}".to_owned())
        })
        .collect::<Vec<_>>();
    let agent = Agent::new(ScriptedTurnToolProvider::new(vec![calls]), registry);
    let mut session = Session::new("mock-parallel-read-lane", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("inspect six independent files"),
            scripted_run_options(3),
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 4);
    assert_eq!(
        settled_tool_results(&session)
            .into_iter()
            .map(|(call_id, _)| call_id)
            .collect::<Vec<_>>(),
        (0..6)
            .map(|index| format!("call-{index}"))
            .collect::<Vec<_>>()
    );
    let terminal_audit_order = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.status != ToolExecutionStatus::Started =>
            {
                Some(execution.call_id.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        terminal_audit_order,
        (0..6)
            .map(|index| format!("call-{index}"))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn parallel_tool_body_failure_does_not_cancel_siblings() -> Result<()> {
    let probe = Arc::new(ToolSchedulerProbe::default());
    let mut registry = ToolRegistry::new();
    for (index, name) in ["read_ok_left", "read_fails", "read_ok_right"]
        .into_iter()
        .enumerate()
    {
        registry.register(Arc::new(ScheduledReadTool {
            name: name.to_owned(),
            delay: Duration::from_millis(if index == 1 { 10 } else { 35 }),
            parallel: true,
            mutation_tracking: ToolMutationTracking::None,
            fail: index == 1,
            probe: Arc::clone(&probe),
        }));
    }
    let calls = ["read_ok_left", "read_fails", "read_ok_right"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| (format!("failure-{index}"), name.to_owned(), "{}".to_owned()))
        .collect::<Vec<_>>();
    let agent = Agent::new(ScriptedTurnToolProvider::new(vec![calls]), registry);
    let mut session = Session::new("mock-parallel-body-failure", "mock-model");
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("run independent reads when one fails"),
            scripted_run_options(3),
            &mut handler,
        )
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(probe.peak.load(Ordering::SeqCst), 3);
    let events = probe
        .events
        .lock()
        .expect("scheduler events lock should not be poisoned");
    assert!(events.iter().any(|event| event == "end:read_ok_left"));
    assert!(events.iter().any(|event| event == "end:read_ok_right"));
    assert_eq!(settled_tool_results(&session).len(), 3);
    assert!(output.outcome.tool_errors.iter().any(|error| {
        error.kind == ToolErrorKind::Internal && error.message.contains("scheduled read failed")
    }));
    Ok(())
}

#[tokio::test]
async fn parallel_opt_in_with_unknown_mutation_tracking_remains_exclusive() -> Result<()> {
    let probe = Arc::new(ToolSchedulerProbe::default());
    let mut registry = ToolRegistry::new();
    for name in ["unsafe_parallel_a", "unsafe_parallel_b"] {
        registry.register(Arc::new(ScheduledReadTool {
            name: name.to_owned(),
            delay: Duration::from_millis(25),
            parallel: true,
            mutation_tracking: ToolMutationTracking::Unknown,
            fail: false,
            probe: Arc::clone(&probe),
        }));
    }
    let calls = ["unsafe_parallel_a", "unsafe_parallel_b"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| (format!("unsafe-{index}"), name.to_owned(), "{}".to_owned()))
        .collect::<Vec<_>>();
    let agent = Agent::new(ScriptedTurnToolProvider::new(vec![calls]), registry);
    let mut session = Session::new("mock-unsafe-parallel-opt-in", "mock-model");
    let mut handler = RecordingEventHandler::default();

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("run conservatively classified reads"),
            scripted_run_options(3),
            &mut handler,
        )
        .await?;

    assert_eq!(probe.peak.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn parallel_tool_cancellation_drains_active_bodies_and_starts_no_queued_body() -> Result<()> {
    let probe = Arc::new(ToolSchedulerProbe::default());
    let mut registry = ToolRegistry::new();
    let calls = (0..6)
        .map(|index| {
            let name = format!("cancelled_read_{index}");
            registry.register(Arc::new(ScheduledReadTool {
                name: name.clone(),
                delay: Duration::from_millis(80),
                parallel: true,
                mutation_tracking: ToolMutationTracking::None,
                fail: false,
                probe: Arc::clone(&probe),
            }));
            (format!("cancelled-call-{index}"), name, "{}".to_owned())
        })
        .collect::<Vec<_>>();
    let agent = Agent::new(ScriptedTurnToolProvider::new(vec![calls]), registry);
    let mut session = Session::new("mock-parallel-cancellation", "mock-model");
    let mut handler = RecordingEventHandler::default();
    let cancellation_owner = RunCancellationOwner::new();
    let input = AgentRunInput::user("cancel a bounded read batch")
        .with_cancellation(cancellation_owner.handle());
    {
        let run = agent.run_with_input(&mut session, input, scripted_run_options(3), &mut handler);
        tokio::pin!(run);
        tokio::select! {
            result = &mut run => panic!("run ended before the parallel lane filled: {result:#?}"),
            () = async {
                while probe.active.load(Ordering::SeqCst) < 4 {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
        assert!(cancellation_owner.request_cancel());
        let _ = run.await;
    }

    let starts = probe
        .events
        .lock()
        .expect("scheduler events lock should not be poisoned")
        .iter()
        .filter(|event| event.starts_with("start:"))
        .count();
    assert_eq!(
        starts, 4,
        "queued calls must never start after cancellation"
    );
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    assert!(cancellation_owner.is_quiescent());
    let interrupted = session
        .entries()
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                    if execution.status == ToolExecutionStatus::Interrupted
            )
        })
        .count();
    assert_eq!(interrupted, 2);
    Ok(())
}

#[tokio::test]
async fn exclusive_tool_is_a_barrier_between_parallel_read_lanes() -> Result<()> {
    let probe = Arc::new(ToolSchedulerProbe::default());
    let mut registry = ToolRegistry::new();
    for (name, delay, parallel) in [
        ("read_left_a", 60, true),
        ("read_left_b", 40, true),
        ("read_barrier", 15, false),
        ("read_right", 10, true),
    ] {
        registry.register(Arc::new(ScheduledReadTool {
            name: name.to_owned(),
            delay: Duration::from_millis(delay),
            parallel,
            mutation_tracking: ToolMutationTracking::None,
            fail: false,
            probe: Arc::clone(&probe),
        }));
    }
    let calls = ["read_left_a", "read_left_b", "read_barrier", "read_right"]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            (
                format!("barrier-call-{index}"),
                name.to_owned(),
                "{}".to_owned(),
            )
        })
        .collect::<Vec<_>>();
    let agent = Agent::new(ScriptedTurnToolProvider::new(vec![calls]), registry);
    let mut session = Session::new("mock-exclusive-read-barrier", "mock-model");
    let mut handler = RecordingEventHandler::default();

    agent
        .run_with_input(
            &mut session,
            AgentRunInput::user("exercise the tool scheduling barrier"),
            scripted_run_options(3),
            &mut handler,
        )
        .await?;

    let events = probe
        .events
        .lock()
        .expect("scheduler events lock should not be poisoned")
        .clone();
    let position = |needle: &str| {
        events
            .iter()
            .position(|event| event == needle)
            .unwrap_or_else(|| panic!("missing scheduler event {needle}: {events:?}"))
    };
    assert!(position("end:read_left_a") < position("start:read_barrier"));
    assert!(position("end:read_left_b") < position("start:read_barrier"));
    assert!(position("end:read_barrier") < position("start:read_right"));
    assert_eq!(probe.peak.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test]
async fn plan_review_error_branches_settle_through_the_assistant_batch() -> Result<()> {
    // RFC-0062 11.2/11.5: every model-issued special-tool call that cannot be accepted settles in
    // the same assistant tool-call batch as ordinary results (exactly one durable record, one
    // provider-visible event), never through a per-tool emit outside the allocator.

    // (a) request_plan_review after the routing microturn: no routing decision is pending.
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![(
            "call-plan-review".to_owned(),
            REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
            r#"{"reason_codes":["architectural_tradeoff"]}"#.to_owned(),
        )]]),
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan-review-late", "mock-model");
    let prompt = "explain the routing contract";
    let logical_run_id = "plan-review-late-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(conversation_run_purpose(
                logical_run_id,
                source_turn,
                TaskRoutingPolicy::Manual,
                AutomaticRouteCapability::Unsupported,
                None,
                None,
            ));
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(3), &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_single_settled_result(
        &session,
        "call-plan-review",
        "not available after the routing microturn",
    );
    assert_eq!(tool_result_event_count(&handler, "call-plan-review"), 1);
    assert!(
        output
            .outcome
            .tool_errors
            .iter()
            .any(|error| error.kind == ToolErrorKind::Unsupported)
    );

    // (b) routing microturn with only a task handoff binding: request_plan_review has no plan
    // review binding, so it is rejected into the batch; the microturn filter still suppresses the
    // model surface and the run retries with a typed decision.
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![
            vec![(
                "call-plan-review".to_owned(),
                REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                r#"{"reason_codes":["architectural_tradeoff"]}"#.to_owned(),
            )],
            vec![(
                "call-chat".to_owned(),
                CONTINUE_WITHOUT_TASK_PLANNING_TOOL_NAME.to_owned(),
                r#"{"reason":"does_not_meet_task_planning_criteria"}"#.to_owned(),
            )],
        ]),
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan-review-unbound", "mock-model");
    let prompt = "ship the cross-crate orchestration change";
    let logical_run_id = "plan-review-unbound-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let task_handoff = TaskPlanningHandoffBinding {
        handoff_id: TaskHandoffId::new("handoff-unbound-1")?,
        task_id: TaskId::new("task-unbound-1")?,
        source_turn: source_turn.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: prompt.to_owned(),
        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
        requested_at_ms: 42,
        decided_at_ms: 43,
    };
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(conversation_run_purpose(
            logical_run_id,
            source_turn,
            TaskRoutingPolicy::Auto,
            AutomaticRouteCapability::DirectTask,
            None,
            Some(task_handoff),
        ))
        .with_cancellation(RunCancellationOwner::new().handle());
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(4), &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_single_settled_result(&session, "call-plan-review", "not available for this run");
    assert_eq!(
        tool_result_event_count(&handler, "call-plan-review"),
        0,
        "routing correction remains durable and model-visible without leaking into product events"
    );
    let decisions = session
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            SessionLogEntry::Control(ControlEntry::ConversationRouteDecisionRecorded(decision)) => {
                Some(decision.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].route, ConversationRoute::Chat);

    // (c) plan review run without a draft binding: submit_plan_draft is rejected into the batch.
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![(
            "call-draft".to_owned(),
            SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
            "{}".to_owned(),
        )]]),
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan-review-no-draft", "mock-model");
    let prompt = "propose how to restructure the coordinator";
    let logical_run_id = "plan-review-no-draft-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(AgentRunPurpose::PlanReview(PlanReviewPurposeContext {
                plan_review_id: binding.plan_review_id.clone(),
                attempt_id: binding.attempt_id.clone(),
                plan_id: binding.plan_id.clone(),
                source_turn: source_turn.clone(),
                route_decision_id: Some(binding.decision_id.clone()),
            }));
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(3), &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_single_settled_result(
        &session,
        "call-draft",
        "submit_plan_draft is not available for this run",
    );
    assert_eq!(tool_result_event_count(&handler, "call-draft"), 1);
    assert!(
        output
            .outcome
            .tool_errors
            .iter()
            .any(|error| error.kind == ToolErrorKind::Unsupported)
    );
    Ok(())
}

#[tokio::test]
async fn mixed_special_and_ordinary_results_settle_in_declaration_order() -> Result<()> {
    // RFC-0062 11.2: one assistant response mixing a rejected special tool with an ordinary tool
    // persists both through the single batch settlement in assistant declaration order.
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![
            (
                "call-plan-review".to_owned(),
                REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                r#"{"reason_codes":["architectural_tradeoff"]}"#.to_owned(),
            ),
            (
                "call-echo".to_owned(),
                "echo".to_owned(),
                r#"{"value":"declared second"}"#.to_owned(),
            ),
        ]]),
        registry,
    );
    let mut session = Session::new("mock-mixed-batch", "mock-model");
    let prompt = "explain the routing contract";
    let logical_run_id = "mixed-batch-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(conversation_run_purpose(
                logical_run_id,
                source_turn,
                TaskRoutingPolicy::Manual,
                AutomaticRouteCapability::Unsupported,
                None,
                None,
            ));
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(3), &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    let settled = settled_tool_results(&session);
    assert_eq!(
        settled
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        vec!["call-plan-review", "call-echo"]
    );
    assert!(
        settled[0]
            .1
            .contains("not available after the routing microturn")
    );
    assert!(settled[1].1.contains("declared second"));
    assert_eq!(tool_result_event_count(&handler, "call-plan-review"), 1);
    assert_eq!(tool_result_event_count(&handler, "call-echo"), 1);
    Ok(())
}

#[tokio::test]
async fn accepted_plan_review_terminates_extra_calls_with_explicit_single_settlement() -> Result<()>
{
    // RFC-0063 6.1: once a plan review decision is accepted, every other call in the same response
    // gets an explicit terminal result (audited Cancelled) and every call settles exactly once.
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![
            (
                "call-plan-review".to_owned(),
                REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                r#"{"reason_codes":["architectural_tradeoff"]}"#.to_owned(),
            ),
            (
                "call-extra".to_owned(),
                REQUEST_TASK_PLANNING_TOOL_NAME.to_owned(),
                r#"{"reason_codes":["target_requires_task_planning"]}"#.to_owned(),
            ),
        ]]),
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-plan-review-accepted", "mock-model");
    let prompt = "design the migration path first";
    let logical_run_id = "plan-review-accepted-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let plan_review_binding = test_plan_review_handoff_binding(&source_turn, prompt);
    let task_handoff = TaskPlanningHandoffBinding {
        handoff_id: TaskHandoffId::new("handoff-accepted-1")?,
        task_id: TaskId::new("task-accepted-1")?,
        source_turn: source_turn.clone(),
        parent_session_ref: SessionRef::new_relative("session.jsonl")?,
        objective: prompt.to_owned(),
        policy_snapshot_hash: "sha256:task-routing-v1".to_owned(),
        route_contract_fingerprint: "sha256:test-route-contract-v1".to_owned(),
        requested_at_ms: 42,
        decided_at_ms: 43,
    };
    let input = input
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(conversation_run_purpose(
            logical_run_id,
            source_turn,
            TaskRoutingPolicy::Auto,
            AutomaticRouteCapability::DirectTask,
            Some(plan_review_binding),
            Some(task_handoff),
        ))
        .with_cancellation(RunCancellationOwner::new().handle());
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(3), &mut handler)
        .await?;
    assert!(matches!(
        output.disposition,
        AgentRunDisposition::StartPlanReview(_)
    ));
    assert_single_settled_result(&session, "call-plan-review", "accepted");
    assert_single_settled_result(&session, "call-extra", "ignored");
    // Both calls still settle durably for provider correctness, while the internal routing batch
    // stays out of product events. The Task / Plan Review runtime owns positive user feedback.
    assert_eq!(tool_result_event_count(&handler, "call-plan-review"), 0);
    assert_eq!(tool_result_event_count(&handler, "call-extra"), 0);
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-extra"
                && execution.status == ToolExecutionStatus::Cancelled
    )));
    assert_eq!(
        output.outcome.tool_call_ids,
        vec!["call-plan-review".to_owned(), "call-extra".to_owned()]
    );
    assert!(
        output
            .outcome
            .tool_errors
            .iter()
            .any(|error| error.kind == ToolErrorKind::Unsupported)
    );
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::PlanReviewHandoff
    );
    Ok(())
}

#[tokio::test]
async fn request_user_input_suspends_with_durable_request_and_cancels_extra_calls() -> Result<()> {
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![
            (
                "call-input".to_owned(),
                REQUEST_USER_INPUT_TOOL_NAME.to_owned(),
                serde_json::json!({
                    "prompt": "Choose the compatibility boundary before implementation.",
                    "questions": [{
                        "id": "compatibility",
                        "header": "Compatibility",
                        "question": "Which compatibility target should be preserved?",
                        "required": true,
                        "field": {
                            "kind": "single_select",
                            "options": [
                                {"id": "current", "label": "Current release"},
                                {"id": "legacy", "label": "Legacy sessions"}
                            ],
                            "allow_other": false
                        }
                    }]
                })
                .to_string(),
            ),
            (
                "call-extra".to_owned(),
                "unknown".to_owned(),
                "{}".to_owned(),
            ),
        ]]),
        ToolRegistry::new(),
    );
    let mut session = Session::new("mock-user-input", "mock-model");
    let logical_run_id = "user-input-root";
    let input = AgentRunInput::user("implement the migration")
        .with_logical_run_id(logical_run_id)
        .with_run_purpose(AgentRunPurpose::Conversation(Box::new(
            ConversationPurposeContext {
                root_run_id: logical_run_id.to_owned(),
                source_turn: ConversationTurnRef::new(
                    session.session_scope_id(),
                    "source-message",
                    logical_run_id,
                )?,
                routing_policy: TaskRoutingPolicy::Manual,
                route_capability: AutomaticRouteCapability::Unsupported,
                writable_memory_routing: false,
                task_handoff: None,
                plan_review: None,
                task_continuation: None,
            },
        )));
    let mut handler = RecordingEventHandler::default();

    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(3), &mut handler)
        .await?;

    let AgentRunDisposition::AwaitingUserInput(reference) = output.disposition else {
        panic!("request_user_input should suspend the run");
    };
    assert_eq!(
        output.outcome.terminal_reason,
        AgentRunTerminalReason::AwaitingUserInput
    );
    let state = session
        .user_input_projection()?
        .request(&reference.identity)
        .cloned()
        .expect("durable request should be projected");
    assert_eq!(
        state.requested.request.prompt,
        "Choose the compatibility boundary before implementation."
    );
    assert!(
        settled_tool_results(&session)
            .iter()
            .all(|(call_id, _)| call_id != "call-input")
    );
    assert_single_settled_result(&session, "call-extra", "suspended");
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-input"
                && execution.status == ToolExecutionStatus::Started
    )));
    Ok(())
}

#[tokio::test]
async fn submit_only_plan_finalizer_rejects_non_submit_before_tool_dispatch() -> Result<()> {
    let executions = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ReadPathTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![(
            "call-finalizer-read".to_owned(),
            "read_path".to_owned(),
            r#"{"path":"src/lib.rs"}"#.to_owned(),
        )]]),
        registry,
    );
    let mut session = Session::new("mock-finalizer", "mock-model");
    let input = AgentRunInput::without_persisted_user_message(vec![ModelMessage::user(
        "Submit the draft now.",
    )])
    .with_logical_run_id("submit-only-finalizer")
    .with_plan_review_submit_only();
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(2), &mut handler)
        .await?;

    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "a non-submit call must never enter the registry"
    );
    let result = settled_tool_results(&session)
        .into_iter()
        .find(|(call_id, _)| call_id == "call-finalizer-read")
        .expect("typed protocol result");
    assert!(result.1.contains("submit_only_protocol_violation"));
    assert!(session.entries().iter().any(|entry| matches!(
        entry,
        SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
            if execution.call_id == "call-finalizer-read"
                && execution.status == ToolExecutionStatus::Failed
    )));
    Ok(())
}

#[test]
fn assistant_batch_floor_and_cap_hold_at_128_results() -> Result<()> {
    // RFC-0062 11.2 worst case: 128 results each with safe text keep their deterministic 512 B
    // minimum preview, the batch stays within the 64 KiB cap, and declaration order is preserved.
    let mut session = Session::new("test", "model");
    let mut handler = RecordingEventHandler::default();
    let mut outcome = AgentRunOutcome::default();
    let batch = (0..128)
        .map(|index| {
            let call = crate::ToolCall {
                id: format!("call-{index}"),
                name: "shell".to_owned(),
                args_json: "{}".to_owned(),
            };
            let result = ToolResult::ok(
                format!("call-{index}"),
                "shell",
                "x".repeat(1024),
                ToolResultMeta::default(),
            );
            (call, result)
        })
        .collect::<Vec<_>>();
    crate::agent::tool_results::emit_tool_result_batch(
        &mut session,
        &mut handler,
        &mut outcome,
        batch,
    )?;

    let settled = settled_tool_results(&session);
    assert_eq!(settled.len(), 128);
    assert_eq!(
        settled
            .iter()
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>(),
        (0..128)
            .map(|index| format!("call-{index}"))
            .collect::<Vec<_>>()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let preview_total = settled
        .iter()
        .map(|(_, preview)| preview.len())
        .sum::<usize>();
    assert!(settled.iter().all(|(_, preview)| preview.len() >= 512));
    assert!(preview_total <= 64 * 1024);
    assert_eq!(outcome.tool_call_ids.len(), 128);
    Ok(())
}

#[tokio::test]
async fn run_outcome_reflects_error_cancelled_and_completed_tool_states() -> Result<()> {
    // RFC-0062 11.5: batch settlement folds every result into the run outcome; an ordinary tool
    // error inside the batch must not fail the run, and the outcome keeps error and completed
    // states distinct. (The cancelled state is asserted by
    // accepted_plan_review_terminates_extra_calls_with_explicit_single_settlement.)
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(EchoTool));
    let agent = Agent::new(
        ScriptedTurnToolProvider::new(vec![vec![
            (
                "call-echo".to_owned(),
                "echo".to_owned(),
                r#"{"value":"ok"}"#.to_owned(),
            ),
            (
                "call-plan-review".to_owned(),
                REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
                r#"{"reason_codes":["architectural_tradeoff"]}"#.to_owned(),
            ),
        ]]),
        registry,
    );
    let mut session = Session::new("mock-outcome-batch", "mock-model");
    let prompt = "explain the routing contract";
    let logical_run_id = "outcome-batch-run";
    let input = AgentRunInput::user(prompt);
    let source_turn = ConversationTurnRef::new(
        session.session_scope_id(),
        input
            .persisted_user_message_id
            .clone()
            .expect("direct input owns a message id"),
        logical_run_id,
    )?;
    let input =
        input
            .with_logical_run_id(logical_run_id)
            .with_run_purpose(conversation_run_purpose(
                logical_run_id,
                source_turn,
                TaskRoutingPolicy::Manual,
                AutomaticRouteCapability::Unsupported,
                None,
                None,
            ));
    let mut handler = RecordingEventHandler::default();
    let output = agent
        .run_with_input(&mut session, input, scripted_run_options(3), &mut handler)
        .await?;
    assert_eq!(output.disposition, AgentRunDisposition::FinalAnswer);
    assert_eq!(
        output.outcome.tool_call_ids,
        vec!["call-echo".to_owned(), "call-plan-review".to_owned()]
    );
    assert!(
        output
            .outcome
            .tool_errors
            .iter()
            .any(|error| error.kind == ToolErrorKind::Unsupported)
    );
    assert!(output.outcome.tool_errors.iter().all(|error| {
        error.kind != ToolErrorKind::ApprovalDenied && error.kind != ToolErrorKind::Interrupted
    }));
    assert_eq!(output.outcome.approval_denials, 0);
    assert!(output.outcome.interrupted_tool_calls.is_empty());
    assert_eq!(settled_tool_results(&session).len(), 2);
    Ok(())
}

struct BashUnknownFamilyTool {
    executions: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for BashUnknownFamilyTool {
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

    fn permission_plan(
        &self,
        _ctx: &crate::ToolContext,
        args: &serde_json::Value,
    ) -> Result<crate::ToolPermissionPlanDraft> {
        let command = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing command"))?;
        Ok(crate::ToolPermissionPlanDraft {
            access: ToolAccess::Read,
            operation: crate::ToolOperation::ExecuteReadOnlyCommand,
            effects: BTreeSet::from([crate::ToolPermissionEffect::FileRead]),
            // Unknown command family: the normalized subject is the command itself, not a
            // `family:` stable subject, so the grant must use the CommandFamily scope.
            subjects: vec![ToolSubject::command(command, command)],
            analysis: crate::ToolAnalysisStatus::Complete,
            containment: crate::ExecutionContainmentRequest {
                filesystem: crate::FilesystemContainment::WorkspaceAndScratch,
                network: crate::NetworkContainment::Deny,
                process: crate::ProcessContainment::OwnedTree,
                environment: crate::EnvironmentContainment::Restricted,
                persistent_process: false,
            },
            semantic_scope: Some(crate::ToolSemanticScope::new("workspace_read", 1)),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([
                ("containment_proven".to_owned(), "true".to_owned()),
                (
                    "execution_backend".to_owned(),
                    "test-owned-process".to_owned(),
                ),
                ("execution_profile".to_owned(), "read-offline".to_owned()),
                (
                    "environment_binding".to_owned(),
                    "test-restricted-v1".to_owned(),
                ),
            ]),
            safe_summary: crate::ToolPermissionSummary {
                title: "Run python3 script".to_owned(),
                detail: "workspace read".to_owned(),
                step_count: 1,
                workspace_code_steps: 0,
            },
        })
    }

    async fn execute(
        &self,
        _ctx: ToolContext,
        call_id: String,
        args: serde_json::Value,
    ) -> Result<ToolResult> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        let command = args
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Ok(ToolResult::ok(
            call_id,
            "bash",
            command,
            ToolResultMeta::default(),
        ))
    }
}

struct SessionGrantUnknownFamilyProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Provider for SessionGrantUnknownFamilyProvider {
    fn name(&self) -> &str {
        "mock-session-grant-unknown-family"
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
                let call_id = format!("call-python-{call_number}");
                let command = if call_number == 1 {
                    "python3 scripts/check.py"
                } else {
                    "python3 scripts/check.py --strict"
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

#[tokio::test]
async fn unknown_family_command_variants_share_command_family_session_grant() -> Result<()> {
    // RFC-0062: an unknown-family command (no `family:` stable subject) approved for session
    // must create a CommandFamily grant so argument variants within the same first-two-token
    // family stop prompting, mirroring the known-family cargo check test.
    let provider_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let approvals = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashUnknownFamilyTool {
        executions: Arc::clone(&executions),
    }));
    let agent = Agent::new(
        SessionGrantUnknownFamilyProvider {
            calls: Arc::clone(&provider_calls),
        },
        registry,
    );
    let workspace = tempfile::tempdir()?;
    let store = JsonlSessionStore::new(workspace.path().join("session.jsonl"))?;
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
        permission_mode_override: None,
        memory_config: MemoryConfig::with_enabled(false),
        compaction_config: CompactionConfig::default(),
        tool_authority: None,
    };
    let mut session = Session::load_from_store(
        "mock-session-grant-unknown-family",
        "mock-model",
        store.clone(),
    )?;
    let mut handler = RecordingEventHandler::default();
    let mut approval_handler = ApproveForSessionHandler {
        approvals: Arc::clone(&approvals),
    };

    let first = agent
        .run_with_approval(
            &mut session,
            "run the check script",
            run_options(),
            &mut handler,
            &mut approval_handler,
        )
        .await?;
    drop(session);
    let mut session =
        Session::load_from_store("mock-session-grant-unknown-family", "mock-model", store)?;
    let second = agent
        .run_with_approval(
            &mut session,
            "run the check script with strict flags",
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
                if grant.source_call_id == "call-python-1"
                    && grant.scope == crate::ToolApprovalSessionGrantScope::CommandFamily {
                        prefix: "python3 scripts/check.py".to_owned()
                    }
        )
    }));
    assert!(!session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval))
                if approval.call_id == "call-python-2"
                    && approval.action == ToolApprovalAuditAction::Requested
        )
    }));
    assert!(session.entries().iter().any(|entry| {
        matches!(
            entry,
            SessionLogEntry::Control(ControlEntry::ToolPermissionDecisionV2(decision))
                if decision.call_id == "call-python-2"
                    && decision.policy_decision == ApprovalMode::Allow
                    && decision.allow_source == Some(ToolApprovalAllowSource::SessionGrant)
                    && decision.grant_id.is_some()
        )
    }));
    Ok(())
}
