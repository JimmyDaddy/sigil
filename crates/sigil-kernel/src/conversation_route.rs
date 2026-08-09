use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    ControlEntry, ConversationTurnRef, PlanId, SessionRef, TaskId, TaskRoutingPolicy, ToolAccess,
    ToolCall, ToolCategory, ToolPreviewCapability, ToolSpec, stable_event_hash, stable_event_uuid,
};

pub const REQUEST_PLAN_REVIEW_TOOL_NAME: &str = "request_plan_review";
pub const SUBMIT_PLAN_DRAFT_TOOL_NAME: &str = "submit_plan_draft";
pub const MAX_PLAN_REVIEW_REASON_CODES: usize = 6;

/// Domain separators for retry-stable plan review identities. Each identity kind uses a distinct
/// namespace so a value derived for one kind can never collide with another kind.
pub const CONVERSATION_ROUTE_DECISION_DOMAIN: &str = "sigil-conversation-route-decision-v1";
pub const PLAN_REVIEW_ID_DOMAIN: &str = "sigil-plan-review-v1";
pub const PLAN_REVIEW_ATTEMPT_ID_DOMAIN: &str = "sigil-plan-review-attempt-v1";
pub const PLAN_REVIEW_PLAN_ID_DOMAIN: &str = "sigil-plan-review-plan-v1";
pub const PLAN_REVIEW_ROUTING_POLICY_DOMAIN: &str = "sigil-plan-review-routing-policy-v1";
pub const PLAN_REVIEW_CHILD_SESSION_DOMAIN: &str = "sigil-plan-review-child-session-v1";

/// Stable semantic route chosen by one ordinary conversation turn.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRoute {
    Chat,
    PlanReview,
    Task,
}

impl ConversationRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::PlanReview => "plan_review",
            Self::Task => "task",
        }
    }
}

/// Bounded model-provided reason for choosing a non-chat route.
///
/// The enum is closed: free-text reasoning is never persisted or interpreted by the host.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ConversationRouteReason {
    ExplicitReviewIntent,
    ArchitecturalTradeoff,
    ScopeUncertain,
    HighImpact,
    PermissionBoundary,
    RouteReviewRequired,
}

impl ConversationRouteReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitReviewIntent => "explicit_review_intent",
            Self::ArchitecturalTradeoff => "architectural_tradeoff",
            Self::ScopeUncertain => "scope_uncertain",
            Self::HighImpact => "high_impact",
            Self::PermissionBoundary => "permission_boundary",
            Self::RouteReviewRequired => "route_review_required",
        }
    }
}

/// Route capability derived by the runtime from exact provider/model/build evidence.
///
/// The model cannot modify this tier. `ReviewFirst` keeps automatic routing enabled but does not
/// expose the direct Task decision; `DirectTask` additionally exposes the durable task decision.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AutomaticRouteCapability {
    #[default]
    Unsupported,
    ReviewFirst,
    DirectTask,
}

impl AutomaticRouteCapability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::ReviewFirst => "review_first",
            Self::DirectTask => "direct_task",
        }
    }

    /// Returns true when automatic routing may expose any typed decision tool.
    pub fn routes_automatically(self) -> bool {
        matches!(self, Self::ReviewFirst | Self::DirectTask)
    }

    /// Returns true when the direct durable task decision may be exposed.
    pub fn allows_direct_task(self) -> bool {
        matches!(self, Self::DirectTask)
    }
}

/// Stable identity for one durable conversation route decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct ConversationRouteDecisionId(String);

impl ConversationRouteDecisionId {
    /// Creates a path-safe route decision identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is not a valid stable task-style id.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        TaskId::new(value.clone())?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity for one plan review lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PlanReviewId(String);

impl PlanReviewId {
    /// Creates a path-safe plan review identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is not a valid stable task-style id.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        TaskId::new(value.clone())?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity for one plan review attempt under a plan review lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(transparent)]
pub struct PlanReviewAttemptId(String);

impl PlanReviewAttemptId {
    /// Creates a path-safe plan review attempt identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier is not a valid stable task-style id.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        TaskId::new(value.clone())?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Append-only root record for one durable conversation route decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ConversationRouteDecisionRecordedEntry {
    pub decision_id: ConversationRouteDecisionId,
    pub source_turn: ConversationTurnRef,
    pub route: ConversationRoute,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_codes: Vec<ConversationRouteReason>,
    pub configured_policy: TaskRoutingPolicy,
    pub effective_capability: AutomaticRouteCapability,
    pub policy_snapshot_hash: String,
    pub route_contract_fingerprint: String,
    pub decided_at_ms: u64,
}

/// Source that admitted one plan review attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewSource {
    ExplicitPlanCommand,
    AutomaticConversationRoute,
}

impl PlanReviewSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ExplicitPlanCommand => "explicit_plan_command",
            Self::AutomaticConversationRoute => "automatic_conversation_route",
        }
    }
}

/// Durable lifecycle status of one plan review attempt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewAttemptStatus {
    Started,
    DraftReady,
    CompletedWithoutDraft,
    Failed,
    Interrupted,
    Cancelled,
}

impl PlanReviewAttemptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::DraftReady => "draft_ready",
            Self::CompletedWithoutDraft => "completed_without_draft",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::CompletedWithoutDraft | Self::Failed | Self::Interrupted | Self::Cancelled
        )
    }
}

/// Terminal reason for a plan review attempt that ended without a user-facing draft.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewTerminalReason {
    NoDraftAfterRetry,
    RunFailed,
    RunInterrupted,
    UserCancelled,
    RejectedAfterDraft,
    SavedOnly,
    RevisionRequested,
    AcceptedAndTaskCreated,
    PlanSuperseded,
}

impl PlanReviewTerminalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoDraftAfterRetry => "no_draft_after_retry",
            Self::RunFailed => "run_failed",
            Self::RunInterrupted => "run_interrupted",
            Self::UserCancelled => "user_cancelled",
            Self::RejectedAfterDraft => "rejected_after_draft",
            Self::SavedOnly => "saved_only",
            Self::RevisionRequested => "revision_requested",
            Self::AcceptedAndTaskCreated => "accepted_and_task_created",
            Self::PlanSuperseded => "plan_superseded",
        }
    }
}

/// Append-only durable record for one plan review attempt lifecycle transition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewAttemptEntry {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source: PlanReviewSource,
    pub source_turn: ConversationTurnRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_decision_id: Option<ConversationRouteDecisionId>,
    /// Retry-stable child session that owns the read-only plan review transcript.
    pub child_session_ref: SessionRef,
    pub status: PlanReviewAttemptStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<PlanReviewTerminalReason>,
    pub recorded_at_ms: u64,
}

/// Host-bound identity for one possible automatic PlanReview decision.
///
/// Created before the routing microturn so the same source turn always derives the same plan
/// review identity. The model receives only the typed tool; it never sees or constructs these ids.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewHandoffBinding {
    pub decision_id: ConversationRouteDecisionId,
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source_turn: ConversationTurnRef,
    /// Exact persisted objective of the source turn; used to fail closed on source drift.
    pub objective: String,
    pub policy_snapshot_hash: String,
    pub route_contract_fingerprint: String,
    pub requested_at_ms: u64,
    pub decided_at_ms: u64,
}

impl PlanReviewHandoffBinding {
    pub fn validate_shape(&self) -> Result<()> {
        if self.policy_snapshot_hash.is_empty() {
            bail!("plan review handoff binding requires a policy snapshot hash");
        }
        if self.route_contract_fingerprint.is_empty() {
            bail!("plan review handoff binding requires a route contract fingerprint");
        }
        Ok(())
    }
}

/// Derives the route decision identity for one source turn.
///
/// The identity is retry-stable: the same exact persisted user turn and logical run always derive
/// the same decision id, so a crash between the routing microturn and the next record cannot
/// produce a second conflicting decision.
#[must_use]
pub fn conversation_route_decision_id_for_source(
    source_turn: &ConversationTurnRef,
) -> ConversationRouteDecisionId {
    ConversationRouteDecisionId(stable_event_uuid(
        CONVERSATION_ROUTE_DECISION_DOMAIN,
        &format!(
            "{}|{}|{}",
            source_turn.session_scope_id, source_turn.message_id, source_turn.logical_run_id
        ),
    ))
}

/// Derives the plan review identity for one source turn (automatic route).
#[must_use]
pub fn plan_review_id_for_source(source_turn: &ConversationTurnRef) -> PlanReviewId {
    PlanReviewId(stable_event_uuid(
        PLAN_REVIEW_ID_DOMAIN,
        &format!(
            "{}|{}|{}",
            source_turn.session_scope_id, source_turn.message_id, source_turn.logical_run_id
        ),
    ))
}

/// Derives a retry-stable plan review identity for an explicit plan command.
///
/// Explicit `/plan` has no persisted provider-visible user turn, so the identity is bound to the
/// session scope and the root logical run of the plan review submission.
#[must_use]
pub fn plan_review_id_for_explicit_command(
    session_scope_id: &str,
    logical_run_id: &str,
) -> PlanReviewId {
    PlanReviewId(stable_event_uuid(
        PLAN_REVIEW_ID_DOMAIN,
        &format!("explicit|{session_scope_id}|{logical_run_id}"),
    ))
}

/// Derives the first attempt identity for a plan review lifecycle.
#[must_use]
pub fn plan_review_attempt_id_for_review(plan_review_id: &PlanReviewId) -> PlanReviewAttemptId {
    PlanReviewAttemptId(stable_event_uuid(
        PLAN_REVIEW_ATTEMPT_ID_DOMAIN,
        &format!("{}|attempt|1", plan_review_id.as_str()),
    ))
}

/// Derives the next attempt identity for a revision under the same plan review lifecycle.
#[must_use]
pub fn plan_review_attempt_id_for_revision(
    plan_review_id: &PlanReviewId,
    previous_attempt_id: &PlanReviewAttemptId,
) -> PlanReviewAttemptId {
    PlanReviewAttemptId(stable_event_uuid(
        PLAN_REVIEW_ATTEMPT_ID_DOMAIN,
        &format!(
            "{}|revision|{}",
            plan_review_id.as_str(),
            previous_attempt_id.as_str()
        ),
    ))
}

/// Derives the plan artifact identity for one plan review attempt.
///
/// Plan review plans are identity-bound (not content-bound): the same attempt always produces the
/// same plan id, while the plan hash still binds the exact draft content for stale decisions.
#[must_use]
pub fn plan_review_plan_id_for_attempt(
    plan_review_id: &PlanReviewId,
    attempt_id: &PlanReviewAttemptId,
) -> PlanId {
    PlanId::new(stable_event_uuid(
        PLAN_REVIEW_PLAN_ID_DOMAIN,
        &format!("{}|{}", plan_review_id.as_str(), attempt_id.as_str()),
    ))
    .expect("stable event uuid is always a valid plan id")
}

/// Host-bound context for the typed `submit_plan_draft` internal tool.
///
/// Carries the host-derived plan identity and source binding; the model supplies only the
/// structured draft fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PlanReviewDraftContext {
    pub plan_review_id: PlanReviewId,
    pub attempt_id: PlanReviewAttemptId,
    pub plan_id: PlanId,
    pub source: crate::PlanSourceRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<String>,
}

/// Stable model-visible contract for one read-only plan review run.
///
/// The run researches with read-only tools and must close with a typed `submit_plan_draft` call;
/// free-text plans are never guessed into durable artifacts.
#[must_use]
pub fn plan_review_system_prompt_contract_material() -> &'static str {
    "You are running a read-only plan review for the current request. Research the workspace with the read-only tools advertised in this request, then submit one validated plan draft by calling submit_plan_draft with schema_version 2, a summary, executable steps (each with a stable step_id, title, role, depends_on, mode and isolation), target paths relative to the workspace, and suggested checks. Optional intents remain unaccepted proposals. You must not modify the workspace, execute shell commands, spawn agents, or create tasks; the host owns the plan identity, hash, timestamps, permissions and the durable artifact. The user will review the plan and decide whether to create a durable task."
}

/// Stable host-owned contract injected when an automatic plan review run finished without a
/// typed draft; the retry is bounded to one additional turn.
#[must_use]
pub fn plan_review_no_draft_retry_contract_material() -> &'static str {
    "The plan review run did not submit a typed plan draft. Automatic plan review requires a valid submit_plan_draft call with at least one executable step before returning a final answer. Re-run the read-only research if needed and call submit_plan_draft with schema_version 2, a summary, executable steps, target paths and suggested checks. If you truthfully cannot produce an executable plan for this request, return a short final explanation of why, and the host will close the review without creating a task."
}

/// Derives the retry-stable child session reference for one plan review attempt.
#[must_use]
pub fn plan_review_child_session_ref(
    plan_review_id: &PlanReviewId,
    attempt_id: &PlanReviewAttemptId,
) -> SessionRef {
    let file_name = stable_event_uuid(
        PLAN_REVIEW_CHILD_SESSION_DOMAIN,
        &format!("{}|{}", plan_review_id.as_str(), attempt_id.as_str()),
    );
    SessionRef::new_relative(format!("children/plan-reviews/{file_name}.jsonl"))
        .expect("plan review child session ref is always relative and safe")
}

/// Computes the policy snapshot hash for automatic plan review routing.
#[must_use]
pub fn plan_review_policy_snapshot_hash() -> String {
    stable_event_hash(
        "sigil-plan-review-routing-policy-v1\0routing=auto\0plan_review=enabled".as_bytes(),
    )
}

/// Computes the route contract fingerprint over the routing contract, tool surface, and effective
/// capability. Provider/model/build facts are appended by the runtime before the digest is frozen.
#[must_use]
pub fn conversation_route_contract_fingerprint(
    contract_material: &str,
    tool_specs: &[ToolSpec],
    capability: AutomaticRouteCapability,
    host_facts: &[(&str, &str)],
) -> String {
    let tools = tool_specs
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "input_schema": spec.input_schema,
            })
        })
        .collect::<Vec<_>>();
    let mut facts = host_facts
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect::<BTreeMap<_, _>>();
    facts.insert("capability".to_owned(), capability.as_str().to_owned());
    stable_event_hash(
        format!(
            "sigil-conversation-route-contract-v1\0contract={contract_material}\0tools={}\0facts={}",
            serde_json::to_string(&tools).expect("tool schema is serializable"),
            serde_json::to_string(&facts).expect("fingerprint facts are serializable"),
        )
        .as_bytes(),
    )
}

/// Model-visible schema for the internal PlanReview routing decision tool.
#[must_use]
pub fn request_plan_review_tool_spec() -> ToolSpec {
    ToolSpec {
        name: REQUEST_PLAN_REVIEW_TOOL_NAME.to_owned(),
        description: "Request a read-only plan review for the current user turn before any execution. Use this when the user wants to see a plan, design, impact analysis, or execution boundary first; when the goal contains significant architectural trade-offs, uncertain scope, high-impact effects, migration strategy, or acceptance criteria that need confirmation before execution; or when an effective route requires review before a durable task. The host owns the objective, plan review identity, permissions, and plan artifact; this tool only opens a read-only review lifecycle that waits for your decision."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "reason_codes": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_PLAN_REVIEW_REASON_CODES,
                    "uniqueItems": true,
                    "items": {
                        "type": "string",
                        "enum": [
                            "explicit_review_intent",
                            "architectural_tradeoff",
                            "scope_uncertain",
                            "high_impact",
                            "permission_boundary",
                            "route_review_required"
                        ]
                    }
                }
            },
            "required": ["reason_codes"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Model-visible schema for the internal `submit_plan_draft` tool.
///
/// The host intercepts and audits this tool; it never enters the ordinary tool registry and never
/// obtains workspace effect. The fenced `sigil-plan-v2` rendering is only a user-visible display
/// format, not a model-to-host control channel.
#[must_use]
pub fn submit_plan_draft_tool_spec() -> ToolSpec {
    ToolSpec {
        name: SUBMIT_PLAN_DRAFT_TOOL_NAME.to_owned(),
        description: "Submit the read-only plan review draft for this request. The draft must use schema_version 2 with a summary, at least one executable step (each with a stable step_id, title, role, depends_on, mode and isolation), target paths relative to the workspace, and suggested checks. Optional intents remain unaccepted proposals: when intents are provided, every write-mode step must bind exactly one intent via intent_aliases, and every alias must reference a top-level intent_alias. The host validates the schema, stable ids, paths, checks and intent proposal; the host owns the plan identity, hash, timestamps and the durable artifact. Do not call this tool when the request cannot be expressed as executable steps."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "schema_version": {"type": "integer", "const": 2},
                "summary": {"type": "string", "minLength": 1},
                "steps": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "step_id": {"type": "string"},
                            "title": {"type": "string"},
                            "display_name": {"type": "string"},
                            "detail": {"type": "string"},
                            "role": {"type": "string", "enum": ["planner", "executor", "subagent_read", "subagent_write"]},
                            "depends_on": {"type": "array", "items": {"type": "string"}},
                            "intent_aliases": {"type": "array", "items": {"type": "string"}},
                            "mode": {"type": "string", "enum": ["read", "write", "review", "verify"]},
                            "isolation": {"type": "string", "enum": ["shared_read_only", "sequential_workspace_write", "changeset_only", "worktree"]},
                            "target_paths": {"type": "array", "items": {"type": "string"}},
                            "suggested_checks": {"type": "array", "items": {"oneOf": [
                                {"type": "string"},
                                {"type": "object", "properties": {"check_spec_id": {"type": "string"}, "command": {"type": "string"}, "args": {"type": "array", "items": {"type": "string"}}, "cwd": {"type": "string"}, "effect": {"type": "string", "enum": ["read_only", "write"]}, "source_line": {"type": "string"}}, "required": ["command"], "additionalProperties": false}
                            ]}},
                            "risk": {"type": "string"},
                            "notes": {"type": "array", "items": {"type": "string"}},
                            "acceptance": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["title"],
                        "additionalProperties": false
                    }
                },
                "intents": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "intent_alias": {"type": "string"},
                            "title": {"type": "string"},
                            "statement": {"type": "string"},
                            "acceptance_criteria": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "criterion_alias": {"type": "string"},
                                        "statement": {"type": "string"},
                                        "required": {"type": "boolean"}
                                    },
                                    "required": ["criterion_alias", "statement", "required"],
                                    "additionalProperties": false
                                }
                            },
                            "depends_on_aliases": {"type": "array", "items": {"type": "string"}}
                        },
                        "required": ["intent_alias", "title", "statement", "acceptance_criteria"],
                        "additionalProperties": false
                    }
                },
                "target_paths": {"type": "array", "minItems": 1, "items": {"type": "string"}},
                "suggested_checks": {
                    "type": "array",
                    "items": {"oneOf": [
                        {"type": "string"},
                        {"type": "object", "properties": {
                            "check_spec_id": {"type": "string"},
                            "command": {"type": "string"},
                            "args": {"type": "array", "items": {"type": "string"}},
                            "cwd": {"type": "string"},
                            "effect": {"type": "string", "enum": ["read_only", "write"]},
                            "source_line": {"type": "string"}
                        }, "required": ["command"], "additionalProperties": false}
                    ]}
                },
                "risk": {"type": "string"},
                "notes": {"type": "array", "items": {"type": "string"}}
            },
            "required": ["schema_version", "summary", "steps", "target_paths", "suggested_checks"],
            "additionalProperties": false
        }),
        category: ToolCategory::Custom,
        access: ToolAccess::Read,
        network_effect: None,
        preview: ToolPreviewCapability::None,
    }
}

/// Stable model-visible policy for the three-way routing-only microturn.
///
/// The contract text is capability-independent; the tool surface actually exposed in the request
/// defines which decisions are possible, and the effective capability is part of the route
/// fingerprint. The host never classifies prompts by keywords.
#[must_use]
pub fn conversation_route_routing_contract_material() -> &'static str {
    r#"You are the semantic conversation router for the current user turn. This is a routing-only microturn: do not answer the user, do not inspect the workspace, and do not use ordinary tools.

Classify the requested outcome by its meaning, not by keywords or by whether the user explicitly mentioned plans, tasks, or commits. Judge the structure of the requested outcome, not its estimated effort or the number of files that may need to be read. Call exactly one of the routing tools advertised in this request and then stop.

When remember_user_preference or remember_project_fact is also advertised and the same user turn explicitly intends a stable preference or project convention to persist beyond this session, call the appropriate remember tool in the same response in addition to the one routing decision. Memory intent is semantic: do not infer it merely from a word or from an ordinary instruction. The remember call still requires host preview and approval, and it must not replace the routing decision.

Call request_plan_review when the user should see and approve a plan before anything executes:
- the user explicitly wants a plan, design, RFC, impact analysis, or execution boundary first;
- the goal contains significant architectural trade-offs, multiple viable directions that materially change the result, uncertain scope, high-impact effects, or migration strategy that must be confirmed;
- the user asked to analyze or propose a batching/delivery strategy without modifying or committing anything;
- acceptance criteria or scope need confirmation before execution.

Call request_task_planning (when available) when the goal is clear and directly executable as a durable multi-step task:
- coordinated changes across multiple files, components, or architectural layers that must land consistently;
- two or more independently useful requested outcomes or work streams that can be investigated or implemented separately and then combined;
- a multi-stage implementation whose stages have dependencies, or long-running multi-part verification;
- a user request to finish, land, or deliver a set of existing workspace changes in reviewed batches, even when the words plan, task, or commit do not appear;
- high-risk execution that benefits from a durable reviewed plan but does not require a pre-execution direction choice.

Call continue_existing_task (when available) only when the user is semantically resuming,
finishing, correcting, or following up on the exact current durable Task selected by the host.
The tool has no task-id argument: never use it for an unrelated request or when a new Task or plan
review is required.

Call continue_without_task_planning for one bounded outcome: an explanation, one symbol lookup, one linear call-flow trace or summary of connected code, one narrow read-only query about a single concern, or a small single-file edit that does not meet any planning criterion. Reading multiple files as evidence for that one result is still ordinary.

Multiple files alone do not require planning. A single bounded explanation, trace, or summary remains an ordinary conversation when every file read is only supporting evidence for that one result. Conversely, read-only work requires planning or review when the requested product contains separate component investigations, a comparison across those investigations, or a synthesis of independently useful results. A request that only analyzes how to batch or split work, without executing or committing, must go to plan review rather than a durable task. When the user explicitly refuses execution or asks for analysis only, never route to a durable task.

Do not produce free text in this routing microturn. The host will execute any approved memory side effect, then start the plan review lifecycle, the durable planner, or an ordinary conversation turn after your typed decision."#
}

/// Stable host-owned transition contract after the model selects ordinary conversation.
#[must_use]
pub fn direct_conversation_continuation_prompt_contract_material() -> &'static str {
    "The routing-only microturn is complete and the typed decision selected an ordinary conversation turn. Fulfill the original user request now, using the ordinary tools advertised in this request when they are needed. Do not discuss or restate the routing decision, announce future work, or stop at an intention to act. Return a final answer only after the requested outcome is complete or you can truthfully report a concrete blocker."
}

/// Parses the bounded model-owned portion of a plan review request.
///
/// # Errors
///
/// Returns an error for unknown fields/reasons, empty or oversized arrays, or duplicates.
pub fn plan_review_reason_codes(call: &ToolCall) -> Result<Vec<ConversationRouteReason>> {
    if call.name != REQUEST_PLAN_REVIEW_TOOL_NAME {
        bail!("unexpected internal plan review routing tool {}", call.name);
    }
    let args: RawPlanReviewArgs = serde_json::from_str(&call.args_json)
        .map_err(|error| anyhow!("invalid plan review routing arguments: {error}"))?;
    if args.reason_codes.is_empty() {
        bail!("plan review routing request requires at least one reason code");
    }
    if args.reason_codes.len() > MAX_PLAN_REVIEW_REASON_CODES {
        bail!("plan review routing request exceeds {MAX_PLAN_REVIEW_REASON_CODES} reason codes");
    }
    let unique = args.reason_codes.iter().copied().collect::<BTreeSet<_>>();
    if unique.len() != args.reason_codes.len() {
        bail!("plan review routing request contains duplicate reason codes");
    }
    Ok(args.reason_codes)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
struct RawPlanReviewArgs {
    reason_codes: Vec<ConversationRouteReason>,
}

/// Latest durable state for one route decision identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationRouteDecisionProjectionEntry {
    pub decision: Option<ConversationRouteDecisionRecordedEntry>,
    pub duplicate_decisions: usize,
    pub conflict: bool,
}

/// Append-only projection of conversation route decisions.
///
/// One exact source turn may have at most one decision. Duplicate identical facts are idempotent;
/// conflicting facts fail closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationRouteDecisionProjection {
    decisions: BTreeMap<ConversationRouteDecisionId, ConversationRouteDecisionProjectionEntry>,
    source_decisions: BTreeMap<(String, String), ConversationRouteDecisionId>,
    pub conflicts: Vec<String>,
}

impl ConversationRouteDecisionProjection {
    /// Replays the append-only control log into the projection.
    #[must_use]
    pub fn from_entries(entries: &[crate::SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let crate::SessionLogEntry::Control(
                ControlEntry::ConversationRouteDecisionRecorded(decision),
            ) = entry
            {
                projection.apply(decision);
            }
        }
        projection
    }

    fn apply(&mut self, entry: &ConversationRouteDecisionRecordedEntry) {
        let source_key = (
            entry.source_turn.session_scope_id.clone(),
            entry.source_turn.message_id.clone(),
        );
        let existing = self.decisions.entry(entry.decision_id.clone()).or_default();
        match &existing.decision {
            None => {
                existing.decision = Some(entry.clone());
                if let Some(previous_id) = self
                    .source_decisions
                    .insert(source_key, entry.decision_id.clone())
                    && previous_id != entry.decision_id
                {
                    existing.conflict = true;
                    self.conflicts.push(format!(
                        "source turn {}:{} has decisions {} and {}",
                        entry.source_turn.session_scope_id,
                        entry.source_turn.message_id,
                        previous_id.as_str(),
                        entry.decision_id.as_str()
                    ));
                }
            }
            Some(previous) => {
                if previous == entry {
                    existing.duplicate_decisions = existing.duplicate_decisions.saturating_add(1);
                } else {
                    existing.conflict = true;
                    self.conflicts.push(format!(
                        "decision {} has conflicting durable facts",
                        entry.decision_id.as_str()
                    ));
                }
            }
        }
    }

    /// Returns the decision for one identity, if any.
    #[must_use]
    pub fn decision(
        &self,
        decision_id: &ConversationRouteDecisionId,
    ) -> Option<&ConversationRouteDecisionRecordedEntry> {
        self.decisions
            .get(decision_id)
            .and_then(|entry| entry.decision.as_ref())
    }

    /// Returns the decision bound to one exact source turn, if any.
    #[must_use]
    pub fn decision_for_source(
        &self,
        source_turn: &ConversationTurnRef,
    ) -> Option<&ConversationRouteDecisionRecordedEntry> {
        let key = (
            source_turn.session_scope_id.clone(),
            source_turn.message_id.clone(),
        );
        self.source_decisions
            .get(&key)
            .and_then(|decision_id| self.decision(decision_id))
    }

    /// Returns the decision id bound to one exact source turn, if any.
    #[must_use]
    pub fn decision_id_for_source(
        &self,
        source_turn: &ConversationTurnRef,
    ) -> Option<&ConversationRouteDecisionId> {
        self.source_decisions.get(&(
            source_turn.session_scope_id.clone(),
            source_turn.message_id.clone(),
        ))
    }

    /// Returns true when any conflicting durable fact was observed.
    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Latest durable state for one plan review lifecycle.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanReviewProjectionEntry {
    pub attempts: Vec<PlanReviewAttemptEntry>,
    pub duplicates: usize,
    pub conflicts: Vec<String>,
}

fn legal_same_attempt_transition(
    previous: PlanReviewAttemptStatus,
    next: PlanReviewAttemptStatus,
) -> bool {
    previous == PlanReviewAttemptStatus::Started
        && matches!(
            next,
            PlanReviewAttemptStatus::DraftReady
                | PlanReviewAttemptStatus::CompletedWithoutDraft
                | PlanReviewAttemptStatus::Failed
                | PlanReviewAttemptStatus::Interrupted
                | PlanReviewAttemptStatus::Cancelled
        )
}

impl PlanReviewProjectionEntry {
    /// Returns the most recently recorded attempt.
    #[must_use]
    pub fn latest_attempt(&self) -> Option<&PlanReviewAttemptEntry> {
        self.attempts.last()
    }

    /// Returns the latest terminal status, if the lifecycle reached a terminal attempt.
    #[must_use]
    pub fn terminal(&self) -> Option<(PlanReviewAttemptStatus, Option<PlanReviewTerminalReason>)> {
        self.attempts
            .iter()
            .rev()
            .find(|attempt| attempt.status.is_terminal())
            .map(|attempt| (attempt.status, attempt.terminal_reason))
    }

    /// Returns true when any recorded attempt carries a terminal status.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        self.terminal().is_some()
    }

    /// Returns the latest attempt whose status is not terminal.
    #[must_use]
    pub fn latest_active_attempt(&self) -> Option<&PlanReviewAttemptEntry> {
        self.attempts.iter().rev().find(|attempt| {
            !attempt.status.is_terminal()
                && !self.attempts.iter().any(|existing| {
                    existing.attempt_id == attempt.attempt_id && existing.status.is_terminal()
                })
        })
    }
}

/// Append-only projection of plan review attempt lifecycles.
///
/// Transitions are strictly validated: every status change must follow a valid prefix, duplicates
/// of the same attempt facts are idempotent, and conflicting facts fail closed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanReviewProjection {
    reviews: BTreeMap<PlanReviewId, PlanReviewProjectionEntry>,
    attempts: BTreeMap<PlanReviewAttemptId, PlanReviewId>,
    decision_reviews: BTreeMap<ConversationRouteDecisionId, PlanReviewId>,
    pub conflicts: Vec<String>,
}

impl PlanReviewProjection {
    /// Replays the append-only control log into the projection.
    #[must_use]
    pub fn from_entries(entries: &[crate::SessionLogEntry]) -> Self {
        let mut projection = Self::default();
        for entry in entries {
            if let crate::SessionLogEntry::Control(ControlEntry::PlanReviewAttempt(attempt)) = entry
            {
                projection.apply(attempt);
            }
        }
        projection
    }

    fn apply(&mut self, entry: &PlanReviewAttemptEntry) {
        let review = self
            .reviews
            .entry(entry.plan_review_id.clone())
            .or_default();
        if let Some(previous_id) = self
            .attempts
            .insert(entry.attempt_id.clone(), entry.plan_review_id.clone())
            && previous_id != entry.plan_review_id
        {
            let conflict = format!(
                "attempt {} is bound to plan reviews {} and {}",
                entry.attempt_id.as_str(),
                previous_id.as_str(),
                entry.plan_review_id.as_str()
            );
            review.conflicts.push(conflict.clone());
            self.conflicts.push(conflict);
        }
        if let Some(route_decision_id) = entry.route_decision_id.as_ref()
            && let Some(previous_review) = self
                .decision_reviews
                .insert(route_decision_id.clone(), entry.plan_review_id.clone())
            && previous_review != entry.plan_review_id
        {
            let conflict = format!(
                "route decision {} is bound to plan reviews {} and {}",
                route_decision_id.as_str(),
                previous_review.as_str(),
                entry.plan_review_id.as_str()
            );
            review.conflicts.push(conflict.clone());
            self.conflicts.push(conflict);
        }
        let same_as_last = review.attempts.last().is_some_and(|last| last == entry);
        if same_as_last {
            review.duplicates = review.duplicates.saturating_add(1);
            return;
        }
        if let Some(previous) = review.attempts.last()
            && previous.attempt_id == entry.attempt_id
            && !legal_same_attempt_transition(previous.status, entry.status)
        {
            let conflict = format!(
                "attempt {} has conflicting lifecycle facts",
                entry.attempt_id.as_str()
            );
            review.conflicts.push(conflict.clone());
            self.conflicts.push(conflict);
        }
        review.attempts.push(entry.clone());
    }

    /// Returns the projection entry for one plan review lifecycle.
    #[must_use]
    pub fn review(&self, plan_review_id: &PlanReviewId) -> Option<&PlanReviewProjectionEntry> {
        self.reviews.get(plan_review_id)
    }

    /// Returns the plan review lifecycle bound to one route decision.
    #[must_use]
    pub fn review_for_decision(
        &self,
        decision_id: &ConversationRouteDecisionId,
    ) -> Option<&PlanReviewProjectionEntry> {
        self.decision_reviews
            .get(decision_id)
            .and_then(|review_id| self.reviews.get(review_id))
    }

    /// Returns the plan review id bound to one route decision.
    #[must_use]
    pub fn plan_review_id_for_decision(
        &self,
        decision_id: &ConversationRouteDecisionId,
    ) -> Option<&PlanReviewId> {
        self.decision_reviews.get(decision_id)
    }

    /// Returns the most recent attempt of one plan review lifecycle.
    #[must_use]
    pub fn latest_attempt(&self, plan_review_id: &PlanReviewId) -> Option<&PlanReviewAttemptEntry> {
        self.review(plan_review_id)
            .and_then(|entry| entry.latest_attempt())
    }

    /// Returns true when one plan review lifecycle has reached a terminal attempt.
    #[must_use]
    pub fn is_terminal(&self, plan_review_id: &PlanReviewId) -> bool {
        self.review(plan_review_id)
            .is_some_and(|entry| entry.is_terminal())
    }

    /// Returns the most recent attempt bound to one plan artifact, if any.
    #[must_use]
    pub fn attempt_for_plan(&self, plan_id: &PlanId) -> Option<&PlanReviewAttemptEntry> {
        self.reviews
            .values()
            .flat_map(|review| review.attempts.iter().rev())
            .find(|attempt| attempt.plan_id == *plan_id)
    }

    /// Returns true when any conflicting durable fact was observed.
    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    /// Validates that appending `entry` after the recorded prefix is a legal transition.
    ///
    /// # Errors
    ///
    /// Returns an error when the same attempt identity is reused with conflicting facts or an
    /// illegal status transition, when a new attempt starts while the previous one is still
    /// running, or when a first record is not `Started`.
    pub fn validate_append(&self, entry: &PlanReviewAttemptEntry) -> Result<()> {
        if let Some(previous) = self.latest_attempt(&entry.plan_review_id) {
            if previous.attempt_id == entry.attempt_id {
                if previous == entry {
                    // identical duplicate is idempotent
                    return Ok(());
                }
                let legal_transition = matches!(
                    (previous.status, entry.status),
                    (
                        PlanReviewAttemptStatus::Started,
                        PlanReviewAttemptStatus::DraftReady
                            | PlanReviewAttemptStatus::CompletedWithoutDraft
                            | PlanReviewAttemptStatus::Failed
                            | PlanReviewAttemptStatus::Interrupted
                            | PlanReviewAttemptStatus::Cancelled,
                    )
                );
                if !legal_transition {
                    bail!(
                        "plan review attempt {} has conflicting lifecycle facts",
                        entry.attempt_id.as_str()
                    );
                }
                return Ok(());
            }
            if previous.status.is_terminal() {
                bail!(
                    "plan review {} is already terminal and cannot accept attempt {}",
                    entry.plan_review_id.as_str(),
                    entry.attempt_id.as_str()
                );
            }
            if previous.status != PlanReviewAttemptStatus::DraftReady {
                bail!(
                    "plan review {} cannot start attempt {} while attempt {} is still running",
                    entry.plan_review_id.as_str(),
                    entry.attempt_id.as_str(),
                    previous.attempt_id.as_str()
                );
            }
            if entry.status != PlanReviewAttemptStatus::Started {
                bail!(
                    "plan review attempt {} must start with a started record",
                    entry.attempt_id.as_str()
                );
            }
        } else if entry.status != PlanReviewAttemptStatus::Started {
            bail!(
                "plan review attempt {} must start with a started record",
                entry.attempt_id.as_str()
            );
        }
        Ok(())
    }
}

/// Reconciles plan review attempts after a durable session load.
///
/// Per RFC-0063 recovery rules: a `Started` attempt without a terminal record is closed with
/// `Interrupted` when no draft exists, and promoted to `DraftReady` when the draft was durably
/// committed to the parent projection but the status transition was not. Conflicted projections
/// are left untouched (their conflict is already the fail-closed signal).
pub fn reconcile_plan_review_attempts(session: &mut crate::Session, now_ms: u64) -> Result<()> {
    let projection = PlanReviewProjection::from_entries(session.entries());
    if projection.has_conflicts() {
        return Ok(());
    }
    let plan_projection = session.plan_artifact_projection();
    let mut pending = Vec::new();
    for (plan_review_id, review) in projection.reviews.iter() {
        let Some(attempt) = review.latest_active_attempt() else {
            continue;
        };
        if attempt.status != PlanReviewAttemptStatus::Started {
            continue;
        }
        let has_draft = plan_projection.plans.contains_key(&attempt.plan_id);
        if has_draft
            && review.attempts.iter().any(|entry| {
                entry.attempt_id == attempt.attempt_id
                    && entry.status == PlanReviewAttemptStatus::DraftReady
            })
        {
            continue;
        }
        pending.push((plan_review_id.clone(), attempt.clone(), has_draft));
    }
    for (plan_review_id, attempt, has_draft) in pending {
        let status = if has_draft {
            PlanReviewAttemptStatus::DraftReady
        } else {
            PlanReviewAttemptStatus::Interrupted
        };
        let entry = PlanReviewAttemptEntry {
            plan_review_id: plan_review_id.clone(),
            attempt_id: attempt.attempt_id,
            plan_id: attempt.plan_id,
            source: attempt.source,
            source_turn: attempt.source_turn,
            route_decision_id: attempt.route_decision_id,
            child_session_ref: attempt.child_session_ref,
            status,
            terminal_reason: (!has_draft).then_some(PlanReviewTerminalReason::RunInterrupted),
            recorded_at_ms: now_ms,
        };
        projection.validate_append(&entry)?;
        session.append_control(ControlEntry::PlanReviewAttempt(entry))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/conversation_route_tests.rs"]
mod tests;
