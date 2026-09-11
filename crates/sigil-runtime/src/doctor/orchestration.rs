use sigil_kernel::{MultiAgentMode, RootConfig, TaskRoutingPolicy};

use super::{DoctorReport, DoctorStatus};

/// Reports the three user-facing automatic orchestration facts required by RFC-0063:
///
/// ```text
/// automatic routing: enabled | disabled | unavailable
/// automatic plan review: available | blocked
/// direct task execution: configured | unavailable
/// ```
pub(super) fn check_orchestration_rollout(report: &mut DoctorReport, config: &RootConfig) {
    let manual = config.task.routing_policy == TaskRoutingPolicy::Manual;
    let disabled = !config.task.enabled || manual;
    let decision = if disabled {
        None
    } else {
        Some(crate::new_install_orchestration_rollout_decision_for_config(config))
    };

    match (
        disabled,
        decision.as_ref().map(|value| value.is_qualified()),
    ) {
        (true, _) => {
            report.push(
                DoctorStatus::Ok,
                "orchestration:automatic-routing",
                if manual {
                    "automatic routing is disabled by explicit [task].routing_policy=\"manual\""
                } else {
                    "automatic routing is unavailable because task orchestration is disabled"
                },
            );
            report.push(
                DoctorStatus::Ok,
                "orchestration:plan-review",
                "automatic plan review is blocked because automatic routing is disabled",
            );
            report.push(
                DoctorStatus::Ok,
                "orchestration:direct-task",
                "direct task execution is unavailable because automatic routing is disabled",
            );
        }
        (false, Some(true)) => {
            let digest = decision
                .as_ref()
                .and_then(|value| value.route_identity_digest.clone());
            let route = digest.as_deref().map(short_digest).unwrap_or("unknown");
            report.push(
                DoctorStatus::Ok,
                "orchestration:automatic-routing",
                format!("automatic routing is enabled and matches qualified release route {route}"),
            );
            report.push(
                DoctorStatus::Ok,
                "orchestration:plan-review",
                "automatic plan review is available",
            );
            report.push(
                DoctorStatus::Ok,
                "orchestration:direct-task",
                "direct task execution is configured; dispatch requires routing tools and an attached executor",
            );
        }
        (false, Some(false)) => {
            let reason = decision
                .as_ref()
                .map(|value| value.reason.as_str())
                .unwrap_or("unqualified");
            report.push(
                DoctorStatus::Ok,
                "orchestration:automatic-routing",
                "automatic routing is enabled",
            );
            report.push(
                DoctorStatus::Ok,
                "orchestration:plan-review",
                "automatic plan review is available",
            );
            report.push(
                DoctorStatus::Ok,
                "orchestration:direct-task",
                format!(
                    "direct task execution is configured; dispatch requires routing tools and an attached executor; release evaluation: {reason}"
                ),
            );
            if config.task.multi_agent_mode == MultiAgentMode::Proactive {
                report.push(
                    DoctorStatus::Warn,
                    "orchestration:proactive-agents",
                    "proactive multi-agent spawn is configured without release evaluation evidence; runtime permissions and budgets still apply",
                );
            }
        }
        (false, None) => {
            report.push(
                DoctorStatus::Error,
                "orchestration:automatic-routing",
                "automatic routing is configured but its capability could not be resolved",
            );
        }
    }
}

fn short_digest(value: &str) -> &str {
    value.get(..value.len().min(22)).unwrap_or(value)
}

#[cfg(test)]
#[path = "../tests/doctor_orchestration_rollout_tests.rs"]
mod tests;
