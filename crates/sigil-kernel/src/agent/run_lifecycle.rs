use anyhow::Result;
use serde_json::json;

use crate::{
    event::{DurableEventType, EventClass},
    session::Session,
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
    )
}

fn append_run_lifecycle_event_payload(
    session: &mut Session,
    run_status: &'static str,
    terminal_reason: &'static str,
    final_message_id: Option<&str>,
    tool_calls: usize,
    error: Option<&str>,
) -> Result<()> {
    session.append_durable_event(
        DurableEventType::RunStatusChanged,
        EventClass::Critical,
        json!({
            "run_status": run_status,
            "terminal_reason": terminal_reason,
            "final_message_id": final_message_id,
            "tool_calls": tool_calls,
            "error": error,
        }),
    )?;
    session.append_durable_event(
        DurableEventType::RunFinalized,
        EventClass::Critical,
        json!({
            "run_status": run_status,
            "terminal_reason": terminal_reason,
            "final_message_id": final_message_id,
            "tool_calls": tool_calls,
            "error": error,
        }),
    )?;
    Ok(())
}
