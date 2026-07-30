use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use sigil_kernel::{
    AgentResultContinuationStatus, ControlEntry, IntegrationPromotionStatus, MultiAgentMode,
    OrchestrationEvalObservationV1, OrchestrationHardInvariant, OrchestrationRouteDisabledEntry,
    Session, SessionLogEntry, TaskAdmissionTrigger, TaskConfig, TaskHandoffDecision,
    TaskParticipantAttemptStatus, TaskRoutingPolicy, TaskRunStatus, ToolExecutionStatus,
};

use crate::{agent_tools::WAIT_AGENT_TOOL_NAME, provider_pressure::provider_route_fingerprint};

/// Exact binary identity used by the local route kill switch.
pub const ORCHESTRATION_RUNTIME_BUILD_ID: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    "+",
    env!("SIGIL_RUNTIME_BUILD_GIT_HASH")
);

/// Runtime guard that scopes automatic orchestration to one provider route and build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrchestrationRouteGuard {
    route_fingerprint: String,
    sigil_build: String,
}

impl OrchestrationRouteGuard {
    #[must_use]
    pub fn new(provider_name: &str, model_name: &str, sigil_build: impl Into<String>) -> Self {
        Self {
            route_fingerprint: provider_route_fingerprint(provider_name, model_name),
            sigil_build: sigil_build.into(),
        }
    }

    #[must_use]
    pub fn route_fingerprint(&self) -> &str {
        &self.route_fingerprint
    }

    #[must_use]
    pub fn sigil_build(&self) -> &str {
        &self.sigil_build
    }

    /// Appends the first zero-tolerance invariant as a local route kill switch.
    ///
    /// Existing disablements are returned without appending another fact.
    ///
    /// # Errors
    ///
    /// Returns an error when the disablement payload is invalid or cannot be persisted.
    pub fn enforce(
        &self,
        session: &mut Session,
        disabled_at_ms: u64,
    ) -> Result<Option<OrchestrationRouteDisabledEntry>> {
        if let Some(disabled) = session
            .orchestration_route_disablement_projection()
            .disablement_for(&self.route_fingerprint, &self.sigil_build)
        {
            return Ok(Some(disabled.clone()));
        }
        let Some(invariant) =
            first_orchestration_hard_invariant(&orchestration_observation(session))
        else {
            return Ok(None);
        };
        let disabled = OrchestrationRouteDisabledEntry {
            route_fingerprint: self.route_fingerprint.clone(),
            sigil_build: self.sigil_build.clone(),
            invariant,
            report_handle: format!(
                "session:{}:orchestration-invariant",
                session.session_scope_id()
            ),
            disabled_at_ms,
        };
        disabled.validate()?;
        session.append_control(ControlEntry::OrchestrationRouteDisabled(disabled.clone()))?;
        Ok(Some(disabled))
    }

    #[must_use]
    pub fn effective_policy(
        &self,
        session: &Session,
        configured_policy: TaskRoutingPolicy,
    ) -> TaskRoutingPolicy {
        let disablements = session.orchestration_route_disablement_projection();
        let exact_route_disabled =
            disablements.is_disabled(&self.route_fingerprint, &self.sigil_build);
        let any_route_disabled = session.entries().iter().any(|entry| {
            matches!(
                entry,
                SessionLogEntry::Control(ControlEntry::OrchestrationRouteDisabled(_))
            )
        });
        if configured_policy == TaskRoutingPolicy::Auto
            && (exact_route_disabled
                || (!any_route_disabled
                    && first_orchestration_hard_invariant(&orchestration_observation(session))
                        .is_some()))
        {
            TaskRoutingPolicy::Manual
        } else {
            configured_policy
        }
    }

    /// Returns the effective multi-agent mode after applying the route-local kill switch.
    ///
    /// A disabled proactive route falls back to explicit user or accepted-plan authority.
    /// Explicit and fully disabled configurations are preserved.
    #[must_use]
    pub fn effective_multi_agent_mode(
        &self,
        session: &Session,
        configured_mode: MultiAgentMode,
    ) -> MultiAgentMode {
        if configured_mode == MultiAgentMode::Proactive
            && session
                .orchestration_route_disablement_projection()
                .is_disabled(&self.route_fingerprint, &self.sigil_build)
        {
            MultiAgentMode::ExplicitRequestOnly
        } else {
            configured_mode
        }
    }

    /// Applies the exact route/build kill switch to one runtime-owned task configuration.
    ///
    /// This does not rewrite the user's persisted configuration. It only constrains the
    /// effective configuration used for subsequent inputs in the disabled session.
    pub fn apply_effective_task_config(&self, session: &Session, task: &mut TaskConfig) {
        task.routing_policy = self.effective_policy(session, task.routing_policy);
        task.multi_agent_mode = self.effective_multi_agent_mode(session, task.multi_agent_mode);
    }
}

/// Derives orchestration observations only from typed durable session facts.
#[must_use]
pub fn orchestration_observation(session: &Session) -> OrchestrationEvalObservationV1 {
    let mut observation = OrchestrationEvalObservationV1::default();
    let mut requested_handoffs = BTreeSet::new();
    let mut requested_source_turns = BTreeMap::new();
    let mut model_requested_handoffs = BTreeSet::new();
    let mut resolved_handoffs = BTreeSet::new();
    let mut accepted_model_tasks = BTreeSet::new();
    let mut started_tasks = BTreeSet::new();
    let mut spawn_identities = BTreeSet::new();
    let mut started_continuations = BTreeSet::new();
    let mut merge_effects = BTreeSet::new();
    let mut committed_finals = BTreeSet::new();

    for entry in session.entries() {
        let SessionLogEntry::Control(control) = entry else {
            continue;
        };
        match control {
            ControlEntry::TaskHandoffRequested(request) => {
                let duplicate_id = !requested_handoffs.insert(request.handoff_id.clone());
                let duplicate_source = requested_source_turns
                    .insert(request.source_turn.clone(), request.handoff_id.clone())
                    .is_some_and(|existing| existing != request.handoff_id);
                if duplicate_id || duplicate_source {
                    increment(&mut observation.duplicate_handoffs);
                }
                if request.trigger == TaskAdmissionTrigger::ModelRequested {
                    model_requested_handoffs.insert(request.handoff_id.clone());
                }
            }
            ControlEntry::TaskHandoffResolved(resolution) => {
                if !resolved_handoffs.insert(resolution.handoff_id.clone()) {
                    increment(&mut observation.duplicate_handoffs);
                }
                if resolution.decision == TaskHandoffDecision::Accepted
                    && model_requested_handoffs.contains(&resolution.handoff_id)
                    && let Some(task_id) = &resolution.task_id
                {
                    accepted_model_tasks.insert(task_id.clone());
                }
            }
            ControlEntry::TaskRun(run) if run.status == TaskRunStatus::Started => {
                started_tasks.insert(run.task_id.clone());
            }
            ControlEntry::AgentThreadStarted(thread) => {
                let identity = match (&thread.batch_id, &thread.batch_member_key) {
                    (Some(batch_id), Some(member_key)) => {
                        format!("batch:{}/{}", batch_id.as_str(), member_key.as_str())
                    }
                    _ => format!("thread:{}", thread.thread_id.as_str()),
                };
                if !spawn_identities.insert(identity) {
                    increment(&mut observation.duplicate_spawns);
                }
            }
            ControlEntry::TaskParticipantAttempt(attempt)
                if attempt.status == TaskParticipantAttemptStatus::Started =>
            {
                if !spawn_identities.insert(format!("participant:{}", attempt.attempt_id.as_str()))
                {
                    increment(&mut observation.duplicate_spawns);
                }
            }
            ControlEntry::AgentResultContinuation(continuation)
                if continuation.status == AgentResultContinuationStatus::Started =>
            {
                if !started_continuations.insert(continuation.thread_id.clone()) {
                    increment(&mut observation.duplicate_continuations);
                }
            }
            ControlEntry::IntegrationLaneMemberApplied(applied) => {
                let identity = format!(
                    "lane:{}/{}/{}",
                    applied.plan_id.as_str(),
                    applied.lane_id.as_str(),
                    applied.member_index
                );
                if !merge_effects.insert(identity) {
                    increment(&mut observation.duplicate_merges);
                }
            }
            ControlEntry::IntegrationPromotionRecorded(promotion)
                if promotion.status == IntegrationPromotionStatus::Promoted =>
            {
                let identity = format!("promotion:{}", promotion.plan_id.as_str());
                if !merge_effects.insert(identity) {
                    increment(&mut observation.duplicate_merges);
                }
            }
            ControlEntry::TaskFinalAnswerCommitted(final_answer) => {
                if !committed_finals.insert(final_answer.task_id.clone()) {
                    increment(&mut observation.duplicate_parent_child_finals);
                }
            }
            ControlEntry::ToolExecution(execution)
                if execution.status == ToolExecutionStatus::Started
                    && execution.tool_name == WAIT_AGENT_TOOL_NAME =>
            {
                increment(&mut observation.model_polling_turns);
            }
            _ => {}
        }
    }
    observation.automatic_task_created = accepted_model_tasks
        .iter()
        .any(|task_id| started_tasks.contains(task_id));
    observation
}

fn first_orchestration_hard_invariant(
    observation: &OrchestrationEvalObservationV1,
) -> Option<OrchestrationHardInvariant> {
    [
        (
            observation.duplicate_handoffs,
            OrchestrationHardInvariant::DuplicateHandoff,
        ),
        (
            observation.duplicate_spawns,
            OrchestrationHardInvariant::DuplicateSpawn,
        ),
        (
            observation.duplicate_continuations,
            OrchestrationHardInvariant::DuplicateContinuation,
        ),
        (
            observation.duplicate_merges,
            OrchestrationHardInvariant::DuplicateMerge,
        ),
        (
            observation.permission_monotonicity_violations,
            OrchestrationHardInvariant::PermissionMonotonicityViolation,
        ),
        (
            observation.unknown_effect_replays,
            OrchestrationHardInvariant::UnknownEffectReplay,
        ),
        (
            observation.duplicate_parent_child_finals,
            OrchestrationHardInvariant::ParentChildDuplicateFinal,
        ),
        (
            observation.model_polling_turns,
            OrchestrationHardInvariant::ModelPollingTurn,
        ),
    ]
    .into_iter()
    .find_map(|(count, invariant)| (count > 0).then_some(invariant))
}

fn increment(value: &mut u32) {
    *value = value.saturating_add(1);
}

#[cfg(test)]
#[path = "tests/orchestration_guard_tests.rs"]
mod tests;
