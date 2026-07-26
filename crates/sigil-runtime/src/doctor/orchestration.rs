use sigil_kernel::{MultiAgentMode, RootConfig, TaskRoutingPolicy};

use super::{DoctorReport, DoctorStatus};

pub(super) fn check_orchestration_rollout(report: &mut DoctorReport, config: &RootConfig) {
    if !config.task.enabled {
        report.push(
            DoctorStatus::Ok,
            "orchestration:rollout",
            "task orchestration is disabled by explicit configuration",
        );
        return;
    }
    if config.task.routing_policy == TaskRoutingPolicy::Manual
        && config.task.multi_agent_mode != MultiAgentMode::Proactive
    {
        report.push(
            DoctorStatus::Ok,
            "orchestration:rollout",
            "coarse rollback is active: automatic handoff and proactive agent spawn are disabled",
        );
        return;
    }

    let decision = crate::new_install_orchestration_rollout_decision_for_config(config);
    if decision.is_qualified()
        && config.task.routing_policy == TaskRoutingPolicy::Auto
        && config.task.multi_agent_mode == MultiAgentMode::Proactive
    {
        let route = decision
            .route_identity_digest
            .as_deref()
            .map(short_digest)
            .unwrap_or("unknown");
        report.push(
            DoctorStatus::Ok,
            "orchestration:rollout",
            format!("automatic handoff and proactive agents match qualified release route {route}"),
        );
        return;
    }

    report.push_with_remediation(
        DoctorStatus::Warn,
        "orchestration:rollout",
        format!(
            "explicit orchestration config is preserved but does not match this release default: {}",
            decision.reason
        ),
        Some(
            "set [task].routing_policy=\"manual\" and multi_agent_mode=\"explicit_request_only\" for coarse rollback, or install a newly qualified release",
        ),
    );
}

fn short_digest(value: &str) -> &str {
    value.get(..value.len().min(22)).unwrap_or(value)
}

#[cfg(test)]
#[path = "../tests/doctor_orchestration_rollout_tests.rs"]
mod tests;
