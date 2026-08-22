use sigil_kernel::{PublicProviderTurnRecoveryPhaseV1, PublicProviderTurnRecoveryViewV1, ToolCall};

/// Renders the shared recovery DTO without exposing physical attempt ids or provider internals.
pub(super) fn provider_turn_recovery_label(view: &PublicProviderTurnRecoveryViewV1) -> String {
    match view.phase {
        PublicProviderTurnRecoveryPhaseV1::Waiting => format!(
            "Reconnecting · retry {}/{} scheduled",
            view.active_retry_count, view.active_max_retries
        ),
        PublicProviderTurnRecoveryPhaseV1::Recovering => format!(
            "Reconnecting · retry {}/{}",
            view.active_retry_count, view.active_max_retries
        ),
        PublicProviderTurnRecoveryPhaseV1::Blocked => format!(
            "Provider recovery needs attention · {}",
            provider_turn_recovery_reason_label(view.reason_code.as_deref())
        ),
        PublicProviderTurnRecoveryPhaseV1::Paused => format!(
            "Provider retry paused · {}",
            provider_turn_recovery_reason_label(view.reason_code.as_deref())
        ),
    }
}

fn provider_turn_recovery_reason_label(reason: Option<&str>) -> &'static str {
    match reason {
        Some("provider_retry_budget_exhausted") => "retry limit reached",
        Some("provider_retry_delay_budget_exhausted") => "retry wait limit reached",
        Some("provider_configuration_or_capacity_required") => "check connection or model settings",
        Some("provider_request_requires_attention") => "request needs review",
        Some("effect_reconciliation_required") => "review an external operation",
        Some("provider_output_or_effect_committed") => "review the interrupted response",
        Some("partial_provider_output_requires_review") => "review the interrupted response",
        Some("recovery_material_unavailable") => "recovery material is unavailable",
        Some("recovery_started_without_safe_completion") => "an interrupted request needs review",
        Some("provider_recovery_claimed_elsewhere") => "another recovery owner is active",
        Some("provider_recovery_cancelled") => "cancelled",
        _ => "action required",
    }
}

pub(super) fn notice_is_timeline_worthy(note: &str) -> bool {
    let normalized = note.to_ascii_lowercase();
    [
        "failed",
        "failure",
        "error",
        "denied",
        "timeout",
        "timed out",
        "deadline",
        "exceeded",
        "unavailable",
        "invalid",
        "cancelled",
        "canceled",
        "interrupted",
        "panic",
        "rejected",
        "budget",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

pub(super) fn notice_rejects_current_final_candidate(note: &str) -> bool {
    matches!(
        note,
        "agent delegation required before final answer; retrying with explicit agent-tool instruction"
            | "agent delegation requirement was not satisfied; no final answer was recorded"
            | "pending agent state blocks final answer; continuing"
            | "pending agent state still blocks final answer; ending this run without another provider retry"
            | "recorded run facts added before final answer; continuing"
    )
}

pub(super) fn spawn_agent_profile_id(call: &ToolCall) -> Option<String> {
    if call.name != "spawn_agent" {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(&call.args_json)
        .ok()?
        .get("profile_id")?
        .as_str()
        .filter(|profile_id| !profile_id.is_empty())
        .map(ToOwned::to_owned)
}
