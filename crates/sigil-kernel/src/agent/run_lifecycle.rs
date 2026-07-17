use anyhow::Result;
use serde_json::json;

use crate::{
    event::{DurableEventType, EventClass},
    session::{ControlEntry, Session},
    verification::ReadinessEvaluatedEntry,
};

use super::AgentRunTerminalReason;

pub(super) fn append_run_lifecycle_events(
    session: &mut Session,
    run_status: &'static str,
    terminal_reason: AgentRunTerminalReason,
    final_message_id: Option<&str>,
    tool_calls: usize,
) -> Result<()> {
    append_run_lifecycle_event_payload(
        session,
        run_status,
        terminal_reason.as_str(),
        final_message_id,
        tool_calls,
        None,
        None,
    )
}

pub(super) fn append_completed_run_lifecycle_events(
    session: &mut Session,
    terminal_reason: AgentRunTerminalReason,
    final_message_id: &str,
    tool_calls: usize,
    readiness: ReadinessEvaluatedEntry,
) -> Result<()> {
    append_run_lifecycle_event_payload(
        session,
        "completed",
        terminal_reason.as_str(),
        Some(final_message_id),
        tool_calls,
        None,
        Some(ControlEntry::ReadinessEvaluated(readiness)),
    )
}

pub(super) fn append_failed_run_lifecycle_events(
    session: &mut Session,
    terminal_reason: &'static str,
    tool_calls: usize,
    error: &str,
) -> Result<()> {
    append_run_lifecycle_event_payload(
        session,
        "failed",
        terminal_reason,
        None,
        tool_calls,
        Some(error),
        None,
    )
}

fn append_run_lifecycle_event_payload(
    session: &mut Session,
    run_status: &'static str,
    terminal_reason: &'static str,
    final_message_id: Option<&str>,
    tool_calls: usize,
    error: Option<&str>,
    terminal_control: Option<ControlEntry>,
) -> Result<()> {
    let payload = json!({
        "run_status": run_status,
        "terminal_reason": terminal_reason,
        "final_message_id": final_message_id,
        "tool_calls": tool_calls,
        "error": error,
    });
    session.append_durable_events_with_controls(
        vec![
            (
                DurableEventType::RunStatusChanged,
                EventClass::Critical,
                payload.clone(),
            ),
            (
                DurableEventType::RunFinalized,
                EventClass::Critical,
                payload,
            ),
        ],
        terminal_control.into_iter().collect(),
    )
}
