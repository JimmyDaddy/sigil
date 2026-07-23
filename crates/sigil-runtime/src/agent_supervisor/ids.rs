use anyhow::Result;
use sigil_kernel::{
    AgentProfileId, AgentRole, AgentRouteId, AgentThreadId, TaskId, TaskRouteId, TaskStepId,
    TaskStepSpec,
};

use crate::{BUILD_PROFILE_ID, EXPLORE_PROFILE_ID, PLAN_PROFILE_ID, WORKER_PROFILE_ID};

use super::{hash_text, short_digest};

pub(super) fn profile_id_for_role(role: AgentRole) -> Result<AgentProfileId> {
    match role {
        AgentRole::Planner => AgentProfileId::new(PLAN_PROFILE_ID),
        AgentRole::Executor => AgentProfileId::new(BUILD_PROFILE_ID),
        AgentRole::SubagentRead => AgentProfileId::new(EXPLORE_PROFILE_ID),
        AgentRole::SubagentWrite => AgentProfileId::new(WORKER_PROFILE_ID),
    }
}

pub(super) fn agent_thread_id_for_task_child(
    task_id: &TaskId,
    plan_version: u32,
    step: &TaskStepSpec,
    child_task_id: &TaskId,
) -> Result<AgentThreadId> {
    let hash = hash_text(&format!(
        "{}:{}:{}:{}",
        task_id.as_str(),
        plan_version,
        step.step_id.as_str(),
        child_task_id.as_str()
    ));
    let digest = short_digest(&hash);
    AgentThreadId::new(format!("agent_v{plan_version}_{}", digest))
}

pub fn chat_agent_thread_id_for_call(
    call_id: &str,
    profile_id: &AgentProfileId,
) -> Result<AgentThreadId> {
    let hash = hash_text(&format!("chat:{}:{}", call_id, profile_id.as_str()));
    AgentThreadId::new(format!("agent_chat_{}", short_digest(&hash)))
}

pub(super) fn task_route_id_for_call(
    task_id: &TaskId,
    step_id: &TaskStepId,
    call_id: &str,
) -> Result<TaskRouteId> {
    let digest = hash_text(&format!(
        "{}:{}:{}",
        task_id.as_str(),
        step_id.as_str(),
        call_id
    ));
    TaskRouteId::new(format!("route_{}", short_digest(&digest)))
}

pub(super) fn agent_route_id_for_call(
    thread_id: &AgentThreadId,
    call_id: &str,
) -> Result<AgentRouteId> {
    let digest = hash_text(&format!("{}:{}", thread_id.as_str(), call_id));
    AgentRouteId::new(format!("agent_route_{}", short_digest(&digest)))
}
