use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    ControlEntry, IntegrationPlan, PlanReviewAttemptStatus, PublicRunEventKind,
    TaskParticipantAttemptEntry, TaskParticipantAttemptId, TaskParticipantAttemptStatus,
    TaskParticipantPurpose, TaskPlanEntry, TaskPlanStatus, TaskRunStatus, TaskStepSpec,
    TaskStepStatus,
};

/// Stable public task phase shared by TUI, HTTP, and desktop adapters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicTaskPhase {
    Routing,
    Planning,
    Execution,
    Integration,
    Synthesis,
    Terminal,
}

/// Stable public conversation phase projected from the durable route/plan/task lifecycle.
///
/// Adapters derive UI phases from this projection; neither the model nor the renderer owns it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PublicConversationPhase {
    Routing,
    Chat,
    Planning,
    AwaitingPlanDecision,
    AwaitingUserInput,
    Task,
    Terminal,
}

impl PublicConversationPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routing => "routing",
            Self::Chat => "chat",
            Self::Planning => "planning",
            Self::AwaitingPlanDecision => "awaiting_plan_decision",
            Self::AwaitingUserInput => "awaiting_user_input",
            Self::Task => "task",
            Self::Terminal => "terminal",
        }
    }
}

/// Stable public route decision shared by TUI, HTTP, and desktop adapters.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicConversationRoute {
    Chat,
    PlanReview,
    Task,
}

impl From<crate::ConversationRoute> for PublicConversationRoute {
    fn from(route: crate::ConversationRoute) -> Self {
        match route {
            crate::ConversationRoute::Chat => Self::Chat,
            crate::ConversationRoute::PlanReview => Self::PlanReview,
            crate::ConversationRoute::Task => Self::Task,
        }
    }
}

/// Stable public plan review status projected from the durable attempt lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicPlanReviewStatus {
    Started,
    WaitingForInput,
    Finalizing,
    DraftReady,
    CompileFailed,
    CompletedWithoutDraft,
    Failed,
    Interrupted,
    Cancelled,
}

impl From<PlanReviewAttemptStatus> for PublicPlanReviewStatus {
    fn from(status: PlanReviewAttemptStatus) -> Self {
        match status {
            PlanReviewAttemptStatus::Started => Self::Started,
            PlanReviewAttemptStatus::WaitingForInput => Self::WaitingForInput,
            PlanReviewAttemptStatus::Finalizing => Self::Finalizing,
            PlanReviewAttemptStatus::DraftReady => Self::DraftReady,
            PlanReviewAttemptStatus::CompileFailed => Self::CompileFailed,
            PlanReviewAttemptStatus::CompletedWithoutDraft => Self::CompletedWithoutDraft,
            PlanReviewAttemptStatus::Failed => Self::Failed,
            PlanReviewAttemptStatus::Interrupted => Self::Interrupted,
            PlanReviewAttemptStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// Typed user action offered by a public plan review surface.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicPlanAction {
    Run,
    Save,
    Revise,
    Reject,
}

impl PublicPlanAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Save => "save",
            Self::Revise => "revise",
            Self::Reject => "reject",
        }
    }
}

/// Public source of one plan review lifecycle.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicPlanReviewSource {
    ExplicitPlanCommand,
    AutomaticConversationRoute,
}

impl From<crate::PlanReviewSource> for PublicPlanReviewSource {
    fn from(source: crate::PlanReviewSource) -> Self {
        match source {
            crate::PlanReviewSource::ExplicitPlanCommand => Self::ExplicitPlanCommand,
            crate::PlanReviewSource::AutomaticConversationRoute => Self::AutomaticConversationRoute,
        }
    }
}

/// Bounded public plan review projection with no prompt, transcript, path, ref, or authority.
///
/// Draft-specific fields (`plan_hash`, `summary`, counts) are present only when the latest
/// attempt committed a typed draft; the status always projects, so a durable attempt without a
/// draft (Planning, failed, interrupted, cancelled) stays visible across reloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PublicPlanReview {
    pub plan_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<String>,
    pub status: PublicPlanReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// True when `summary` is a bounded compact projection rather than the complete detail.
    pub summary_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_check_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    pub allowed_actions: Vec<PublicPlanAction>,
    pub source: PublicPlanReviewSource,
    pub stale: bool,
    /// Candidate revision state is projected separately so a failed/running attempt never hides
    /// the immutable active plan above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<PublicPlanRevisionSummaryV1>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct PublicPlanRevisionSummaryV1 {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_ordinal: Option<u32>,
    pub status: PublicPlanRevisionStatusV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
}

/// Stable product-facing phase of a revision while the immutable base plan remains reviewable.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PublicPlanRevisionStatusV1 {
    AwaitingGuidance,
    Queued,
    Researching,
    WaitingForInput,
    Finalizing,
    Failed,
    Cancelled,
    Succeeded,
}

/// Bounded public plan-step DTO with no prompt, transcript, path, ref, or mutation authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct PublicTaskPlanStep {
    pub step_id: String,
    pub title: String,
    pub role: String,
    pub depends_on: Vec<String>,
    pub mode: String,
    pub isolation: String,
}

impl From<&TaskStepSpec> for PublicTaskPlanStep {
    fn from(step: &TaskStepSpec) -> Self {
        Self {
            step_id: step.step_id.as_str().to_owned(),
            title: crate::safe_persistence_text(&step.title),
            role: step.role.as_str().to_owned(),
            depends_on: step
                .depends_on
                .iter()
                .map(|dependency| dependency.as_str().to_owned())
                .collect(),
            mode: step.effective_mode().as_str().to_owned(),
            isolation: task_isolation_label(step).to_owned(),
        }
    }
}

#[derive(Debug, Clone)]
struct PublicIntegrationPlanContext {
    task_id: String,
    plan_version: u32,
    conflicts_by_lane: BTreeMap<String, Vec<String>>,
}

/// Stateful public task-event projection.
///
/// Integration lane records intentionally do not repeat their task identity. This projector
/// retains only bounded plan and attempt identities so external adapters can emit typed events
/// without parsing opaque control payloads or exposing private integration targets.
#[derive(Debug, Clone, Default)]
pub struct PublicTaskEventProjector {
    integration_plans: BTreeMap<String, PublicIntegrationPlanContext>,
    step_attempts:
        BTreeMap<(String, u32, String), (TaskParticipantAttemptId, TaskParticipantAttemptStatus)>,
    plan_reviews: BTreeMap<crate::PlanReviewId, crate::PlanId>,
    user_inputs: crate::UserInputProjectionV1,
}

impl PublicTaskEventProjector {
    /// Projects one control entry into zero or more public task events.
    #[must_use]
    pub fn project_control(&mut self, control: &ControlEntry) -> Vec<PublicRunEventKind> {
        if let Some(entry) = crate::UserInputLifecycleEntryV1::from_control(control) {
            let identity = entry.identity().clone();
            let request_hash = entry.request_hash().to_owned();
            if let Err(error) = self.user_inputs.apply(entry) {
                return vec![PublicRunEventKind::RunFailed {
                    error: format!("invalid durable user input lifecycle: {error}"),
                }];
            }
            let Some(request) = self
                .user_inputs
                .request(&identity)
                .map(crate::UserInputRequestStateV1::public_view)
            else {
                return vec![PublicRunEventKind::RunFailed {
                    error: "durable user input lifecycle lost its projected request".to_owned(),
                }];
            };
            return vec![PublicRunEventKind::UserInputChanged {
                request_id: identity.request_id.as_str().to_owned(),
                generation: identity.generation,
                request_hash,
                status: request.status,
                request: Box::new(request),
            }];
        }
        match control {
            ControlEntry::ConversationRouteDecisionRecorded(entry) => {
                vec![PublicRunEventKind::ConversationRouteChanged {
                    decision_id: entry.decision_id.as_str().to_owned(),
                    route: entry.route.into(),
                    status: "decided".to_owned(),
                }]
            }
            ControlEntry::PlanReviewAttempt(entry) => {
                self.plan_reviews
                    .insert(entry.plan_review_id.clone(), entry.plan_id.clone());
                vec![PublicRunEventKind::PlanReviewChanged {
                    plan_review_id: entry.plan_review_id.as_str().to_owned(),
                    plan_id: entry.plan_id.as_str().to_owned(),
                    status: entry.status.into(),
                }]
            }
            ControlEntry::TaskHandoffRequested(entry) => vec![
                PublicRunEventKind::TaskRoutingChanged {
                    handoff_id: entry.handoff_id.as_str().to_owned(),
                    status: "requested".to_owned(),
                    task_id: None,
                },
                PublicRunEventKind::TaskPhaseChanged {
                    task_id: None,
                    phase: PublicTaskPhase::Routing,
                    status: "requested".to_owned(),
                },
            ],
            ControlEntry::TaskHandoffResolved(entry) => {
                let status = match entry.decision {
                    crate::TaskHandoffDecision::Accepted => "accepted",
                    crate::TaskHandoffDecision::Rejected => "rejected",
                }
                .to_owned();
                let task_id = entry
                    .task_id
                    .as_ref()
                    .map(|task_id| task_id.as_str().to_owned());
                vec![
                    PublicRunEventKind::TaskRoutingChanged {
                        handoff_id: entry.handoff_id.as_str().to_owned(),
                        status: status.clone(),
                        task_id: task_id.clone(),
                    },
                    PublicRunEventKind::TaskPhaseChanged {
                        task_id,
                        phase: PublicTaskPhase::Routing,
                        status,
                    },
                ]
            }
            ControlEntry::TaskRun(entry) => vec![PublicRunEventKind::TaskPhaseChanged {
                task_id: Some(entry.task_id.as_str().to_owned()),
                phase: task_run_phase(entry.status),
                status: task_run_status_label(entry.status).to_owned(),
            }],
            ControlEntry::TaskPlan(entry) => vec![public_task_plan_updated(entry)],
            ControlEntry::TaskStep(entry) => {
                let attempt_id = self
                    .step_attempts
                    .get(&(
                        entry.task_id.as_str().to_owned(),
                        entry.plan_version,
                        entry.step_id.as_str().to_owned(),
                    ))
                    .map(|(attempt_id, _)| attempt_id.as_str().to_owned());
                vec![PublicRunEventKind::TaskStepChanged {
                    task_id: entry.task_id.as_str().to_owned(),
                    plan_version: entry.plan_version,
                    step_id: entry.step_id.as_str().to_owned(),
                    attempt_id,
                    status: task_step_status_label(entry.status).to_owned(),
                }]
            }
            ControlEntry::TaskParticipantAttempt(entry) => self.project_participant(entry),
            ControlEntry::IntegrationPlanRecorded(entry) => {
                self.record_integration_plan(&entry.plan);
                vec![PublicRunEventKind::TaskPhaseChanged {
                    task_id: Some(entry.plan.task_id.as_str().to_owned()),
                    phase: PublicTaskPhase::Integration,
                    status: "planned".to_owned(),
                }]
            }
            ControlEntry::IntegrationLaneChanged(entry) => {
                let Some(plan) = self.integration_plans.get(entry.plan_id.as_str()) else {
                    return Vec::new();
                };
                vec![PublicRunEventKind::IntegrationLaneChanged {
                    task_id: plan.task_id.clone(),
                    plan_version: plan.plan_version,
                    plan_id: entry.plan_id.as_str().to_owned(),
                    lane_id: entry.lane_id.as_str().to_owned(),
                    status: entry.status.as_str().to_owned(),
                    conflicts: plan
                        .conflicts_by_lane
                        .get(entry.lane_id.as_str())
                        .cloned()
                        .unwrap_or_default(),
                }]
            }
            _ => Vec::new(),
        }
    }

    fn project_participant(
        &mut self,
        entry: &TaskParticipantAttemptEntry,
    ) -> Vec<PublicRunEventKind> {
        let task_id = entry.task_id.as_str().to_owned();
        let phase = match entry.purpose {
            TaskParticipantPurpose::Planner => PublicTaskPhase::Planning,
            TaskParticipantPurpose::Step => PublicTaskPhase::Execution,
            TaskParticipantPurpose::Synthesis => PublicTaskPhase::Synthesis,
        };
        let status = task_participant_status_label(entry.status).to_owned();
        let mut events = vec![PublicRunEventKind::TaskPhaseChanged {
            task_id: Some(task_id.clone()),
            phase,
            status: status.clone(),
        }];
        let (Some(plan_version), Some(step_id)) = (entry.plan_version, entry.step_id.as_ref())
        else {
            return events;
        };
        self.step_attempts.insert(
            (task_id.clone(), plan_version, step_id.as_str().to_owned()),
            (entry.attempt_id.clone(), entry.status),
        );
        let (active, completed, failed) = self.task_batch_counts(&task_id, plan_version);
        events.push(PublicRunEventKind::TaskBatchChanged {
            task_id: task_id.clone(),
            plan_version,
            batch_id: format!("{task_id}:plan:{plan_version}:participants"),
            active,
            completed,
            failed,
        });
        events.push(PublicRunEventKind::TaskStepChanged {
            task_id,
            plan_version,
            step_id: step_id.as_str().to_owned(),
            attempt_id: Some(entry.attempt_id.as_str().to_owned()),
            status,
        });
        events
    }

    fn task_batch_counts(&self, task_id: &str, plan_version: u32) -> (u32, u32, u32) {
        self.step_attempts
            .iter()
            .filter(|((candidate_task_id, candidate_plan_version, _), _)| {
                candidate_task_id == task_id && *candidate_plan_version == plan_version
            })
            .fold(
                (0_u32, 0_u32, 0_u32),
                |(active, completed, failed), (_, (_, status))| match status {
                    TaskParticipantAttemptStatus::Started => {
                        (active.saturating_add(1), completed, failed)
                    }
                    TaskParticipantAttemptStatus::Completed => {
                        (active, completed.saturating_add(1), failed)
                    }
                    TaskParticipantAttemptStatus::Failed
                    | TaskParticipantAttemptStatus::Blocked
                    | TaskParticipantAttemptStatus::Cancelled
                    | TaskParticipantAttemptStatus::Interrupted => {
                        (active, completed, failed.saturating_add(1))
                    }
                },
            )
    }

    fn record_integration_plan(&mut self, plan: &IntegrationPlan) {
        let mut conflicts_by_lane = BTreeMap::new();
        for lane in &plan.lanes {
            let proposal_ids = lane
                .proposals
                .iter()
                .map(|proposal| proposal.as_str())
                .collect::<BTreeSet<_>>();
            let conflicts = plan
                .conflicts
                .iter()
                .filter(|conflict| {
                    proposal_ids.contains(conflict.left.as_str())
                        && proposal_ids.contains(conflict.right.as_str())
                })
                .flat_map(|conflict| conflict.reasons.iter().copied())
                .map(|reason| reason.as_str().to_owned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            conflicts_by_lane.insert(lane.lane_id.as_str().to_owned(), conflicts);
        }
        self.integration_plans.insert(
            plan.plan_id.as_str().to_owned(),
            PublicIntegrationPlanContext {
                task_id: plan.task_id.as_str().to_owned(),
                plan_version: plan.plan_version,
                conflicts_by_lane,
            },
        );
    }
}

fn public_task_plan_updated(entry: &TaskPlanEntry) -> PublicRunEventKind {
    PublicRunEventKind::TaskPlanUpdated {
        task_id: entry.task_id.as_str().to_owned(),
        plan_version: entry.plan_version,
        status: task_plan_status_label(entry.status).to_owned(),
        steps: entry.steps.iter().map(PublicTaskPlanStep::from).collect(),
    }
}

fn task_run_phase(status: TaskRunStatus) -> PublicTaskPhase {
    match status {
        TaskRunStatus::Started => PublicTaskPhase::Planning,
        TaskRunStatus::Running | TaskRunStatus::Paused => PublicTaskPhase::Execution,
        TaskRunStatus::Completed
        | TaskRunStatus::Failed
        | TaskRunStatus::Cancelled
        | TaskRunStatus::Interrupted => PublicTaskPhase::Terminal,
    }
}

fn task_run_status_label(status: TaskRunStatus) -> &'static str {
    match status {
        TaskRunStatus::Started => "started",
        TaskRunStatus::Running => "running",
        TaskRunStatus::Paused => "paused",
        TaskRunStatus::Completed => "completed",
        TaskRunStatus::Failed => "failed",
        TaskRunStatus::Cancelled => "cancelled",
        TaskRunStatus::Interrupted => "interrupted",
    }
}

fn task_plan_status_label(status: TaskPlanStatus) -> &'static str {
    match status {
        TaskPlanStatus::Proposed => "proposed",
        TaskPlanStatus::Accepted => "accepted",
        TaskPlanStatus::Superseded => "superseded",
        TaskPlanStatus::Rejected => "rejected",
    }
}

fn task_step_status_label(status: TaskStepStatus) -> &'static str {
    match status {
        TaskStepStatus::Pending => "pending",
        TaskStepStatus::Running => "running",
        TaskStepStatus::Completed => "completed",
        TaskStepStatus::Failed => "failed",
        TaskStepStatus::Blocked => "blocked",
        TaskStepStatus::Cancelled => "cancelled",
        TaskStepStatus::Interrupted => "interrupted",
        TaskStepStatus::Superseded => "superseded",
    }
}

fn task_participant_status_label(status: TaskParticipantAttemptStatus) -> &'static str {
    match status {
        TaskParticipantAttemptStatus::Started => "started",
        TaskParticipantAttemptStatus::Completed => "completed",
        TaskParticipantAttemptStatus::Failed => "failed",
        TaskParticipantAttemptStatus::Blocked => "blocked",
        TaskParticipantAttemptStatus::Cancelled => "cancelled",
        TaskParticipantAttemptStatus::Interrupted => "interrupted",
    }
}

fn task_isolation_label(step: &TaskStepSpec) -> &'static str {
    match step.effective_isolation() {
        crate::TaskIsolationMode::SharedReadOnly => "shared_read_only",
        crate::TaskIsolationMode::SequentialWorkspaceWrite => "sequential_workspace_write",
        crate::TaskIsolationMode::ChangesetOnly => "changeset_only",
        crate::TaskIsolationMode::Worktree => "worktree",
    }
}

#[cfg(test)]
#[path = "tests/public_task_event_tests.rs"]
mod tests;
