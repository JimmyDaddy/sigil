use std::fmt;

use anyhow::Result;
use serde_json::Value;

use crate::{
    ExternalProvenanceEntry, ExternalTrust,
    event::{EventHandler, RunEvent},
    provider::ToolCall,
    session::{ControlEntry, Session, ToolExecutionStatus},
    tool::{ToolErrorKind, ToolResult, ToolResultStatus, ToolSubject},
};

use super::{
    AgentRunOutcome,
    tool_audit::{append_tool_execution_audit, attach_tool_call_context},
};

pub(super) fn record_and_emit_tool_result<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    result: ToolResult,
) -> Result<()>
where
    H: EventHandler,
{
    record_tool_run_outcome(outcome, &result);
    emit_tool_result(session, handler, result)
}

pub(super) fn emit_tool_result<H>(
    session: &mut Session,
    handler: &mut H,
    mut result: ToolResult,
) -> Result<()>
where
    H: EventHandler,
{
    let mut registrations = std::mem::take(&mut result.url_capability_registrations);
    let external_sources = std::mem::take(&mut result.external_sources);
    let message = result.to_model_message();
    for registration in registrations.iter_mut() {
        registration.durable_entry_id.clone_from(&message.id);
    }
    let registrar = session.user_url_capability_registrar();
    if !registrations.is_empty() {
        let registrar = registrar.as_ref().ok_or_else(|| {
            anyhow::anyhow!("tool result produced URL capabilities without a session registrar")
        })?;
        for registration in registrations.iter() {
            if let Err(error) = registrar.stage(registration.clone()) {
                let _ = registrar.rollback_message(&message.id);
                return Err(error);
            }
        }
    }
    if let Err(error) = session.append_tool_message(message.clone()) {
        if !registrations.is_empty()
            && let Some(registrar) = registrar.as_ref()
        {
            let _ = registrar.rollback_message(&message.id);
        }
        return Err(error);
    }
    for registration in registrations.iter() {
        let descriptor = registration.durable_descriptor(session.session_scope_id());
        descriptor.validate()?;
        let control = ControlEntry::WebUrlCapabilityDescriptor(descriptor);
        if let Err(error) = session.append_control(control.clone()) {
            if !registrations.is_empty()
                && let Some(registrar) = registrar.as_ref()
            {
                let _ = registrar.rollback_message(&message.id);
            }
            return Err(error);
        }
        handler.handle(RunEvent::Control(control))?;
    }
    if !external_sources.is_empty() {
        let provenance = ExternalProvenanceEntry {
            session_scope_id: session.session_scope_id().to_owned(),
            message_id: message.id.clone(),
            trust: ExternalTrust::ExternalUntrusted,
            sources: *external_sources,
            citations: Vec::new(),
        };
        provenance.validate_against_message(&message)?;
        let control = ControlEntry::ExternalProvenance(provenance);
        if let Err(error) = session.append_control(control.clone()) {
            if !registrations.is_empty()
                && let Some(registrar) = registrar.as_ref()
            {
                let _ = registrar.rollback_message(&message.id);
            }
            return Err(error);
        }
        handler.handle(RunEvent::Control(control))?;
    }
    if !registrations.is_empty()
        && let Some(registrar) = registrar.as_ref()
        && let Err(error) = registrar.commit_message(&message.id)
    {
        let rollback_error = registrar.rollback_message(&message.id).err();
        return Err(error.context(match rollback_error {
            Some(rollback_error) => format!(
                "failed to commit tool-result URL capabilities; rollback also failed: {rollback_error:#}"
            ),
            None => "failed to commit tool-result URL capabilities".to_owned(),
        }));
    }
    handler.handle(RunEvent::ToolResult(result))
}

pub(super) fn record_tool_run_outcome(outcome: &mut AgentRunOutcome, result: &ToolResult) {
    if !outcome.tool_call_ids.contains(&result.call_id) {
        outcome.tool_call_ids.push(result.call_id.clone());
    }
    if !result.metadata.changed_files.is_empty() {
        for file in &result.metadata.changed_files {
            if !outcome.changed_files.contains(file) {
                outcome.changed_files.push(file.clone());
            }
        }
    }
    let ToolResultStatus::Error(error) = &result.status else {
        return;
    };
    if error.kind == ToolErrorKind::ApprovalDenied {
        outcome.approval_denials += 1;
    }
    if error.kind == ToolErrorKind::Interrupted {
        outcome.interrupted_tool_calls.push(result.call_id.clone());
    }
    outcome.tool_errors.push(error.clone());
}

pub(super) fn append_invalid_tool_input_result<H, E>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    call: &ToolCall,
    subjects: &[ToolSubject],
    error: E,
) -> Result<()>
where
    H: EventHandler,
    E: fmt::Display,
{
    let mut result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::InvalidInput,
        format!("invalid tool arguments for {}: {error}", call.name),
    );
    attach_tool_call_context(&mut result, call, subjects);
    append_tool_execution_audit(
        session,
        call,
        subjects,
        ToolExecutionStatus::Failed,
        None,
        Some(&result),
    )?;
    record_and_emit_tool_result(session, handler, outcome, result)
}

pub(super) fn agent_tool_result_satisfies_delegation(result: &ToolResult) -> bool {
    if result.is_error() {
        return false;
    }
    let details = &result.metadata.details;
    if details.get("thread_id").and_then(Value::as_str).is_some()
        && details.get("status").and_then(Value::as_str).is_some()
    {
        return true;
    }
    if details
        .get("result_available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    details
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(is_terminal_agent_status)
}

fn is_terminal_agent_status(status: &str) -> bool {
    matches!(
        status,
        "completed" | "failed" | "cancelled" | "interrupted" | "closed"
    )
}
