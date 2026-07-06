use sigil_kernel::ToolCall;

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
