use std::fmt;

use anyhow::Result;
use serde_json::Value;

use crate::{
    ExternalProvenanceEntry, ExternalTrust,
    event::{EventHandler, RunEvent},
    provider::ToolCall,
    session::{
        ControlEntry, Session, ToolArtifactSensitivity, ToolExecutionStatus, ToolResultRecordedV3,
    },
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

/// RFC-0062 11.2: settles one assistant tool-call batch with deterministic two-phase preview
/// allocation in assistant declaration order, then appends each result in that order.
pub(super) fn emit_tool_result_batch<H>(
    session: &mut Session,
    handler: &mut H,
    outcome: &mut AgentRunOutcome,
    batch: Vec<(crate::ToolCall, ToolResult)>,
) -> Result<()>
where
    H: EventHandler,
{
    if batch.is_empty() {
        return Ok(());
    }
    let declaration_order = batch
        .iter()
        .map(|(call, _)| call.id.clone())
        .collect::<Vec<_>>();
    let candidate_bytes = batch
        .iter()
        .map(|(call, result)| {
            let cap = crate::tool_model_view_initial_limit(&result.tool_name);
            let safe_len = crate::safe_persistence_text(&result.content).len();
            (call.id.clone(), safe_len.min(cap))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let limits = crate::allocate_batch_preview_limits(&declaration_order, &candidate_bytes);
    let mut first_error = None;
    for (_, result) in batch {
        let limit = limits
            .get(&result.call_id)
            .copied()
            .unwrap_or_else(|| crate::tool_model_view_initial_limit(&result.tool_name));
        record_tool_run_outcome(outcome, &result);
        if let Err(error) = emit_tool_result_with_limit(session, handler, result, limit) {
            if first_error.is_none() {
                first_error = Some(error);
            }
            // RFC-0062 11.5: a settlement failure must not leave completed tool results
            // un-consumed; keep draining the remaining batch so child threads and their
            // supervisor reservations settle, then report the first failure.
            continue;
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(super) fn emit_tool_result<H>(
    session: &mut Session,
    handler: &mut H,
    result: ToolResult,
) -> Result<()>
where
    H: EventHandler,
{
    let limit = crate::tool_model_view_initial_limit(&result.tool_name);
    emit_tool_result_with_limit(session, handler, result, limit)
}

fn emit_tool_result_with_limit<H>(
    session: &mut Session,
    handler: &mut H,
    mut result: ToolResult,
    model_preview_limit: usize,
) -> Result<()>
where
    H: EventHandler,
{
    let mut registrations = std::mem::take(&mut result.url_capability_registrations);
    let external_sources = std::mem::take(&mut result.external_sources);
    let bundled_receipts = std::mem::take(&mut result.control_entries);
    if bundled_receipts
        .iter()
        .any(|control| !matches!(control, ControlEntry::ToolArtifactRead(_)))
    {
        anyhow::bail!("tool result contains an unsupported deferred control entry");
    }
    let sensitivity = if external_sources.is_empty() {
        ToolArtifactSensitivity::Ordinary
    } else {
        ToolArtifactSensitivity::ExternalUntrusted
    };
    let artifact_store = session.tool_artifact_store();
    let (recorded, display) = if let Some(recorded) = result.durable_v3_projection() {
        // RFC-0062 8/11.2: harness-owned process capture already published the artifact; the
        // pre-settled projection is re-projected against the batch-allocated preview budget so
        // the provider never sees more than the allocator awarded.
        let mut recorded = recorded.clone();
        recorded.reproject_preview(
            model_preview_limit,
            crate::ToolPreviewTruncationReasonV1::BatchBudget,
        )?;
        let display = recorded.display_view();
        (recorded, display)
    } else {
        ToolResultRecordedV3::capture_with_model_preview_limit(
            &result,
            artifact_store.as_ref(),
            sensitivity,
            model_preview_limit,
            None,
        )?
    };
    let message = recorded.model_message()?;
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
    let mut controls = Vec::with_capacity(
        bundled_receipts.len() + registrations.len() + usize::from(!external_sources.is_empty()),
    );
    controls.extend(bundled_receipts);
    for registration in registrations.iter() {
        let descriptor = registration.durable_descriptor(session.session_scope_id());
        controls.push(ControlEntry::WebUrlCapabilityDescriptor(descriptor));
    }
    if !external_sources.is_empty() {
        let provenance = ExternalProvenanceEntry {
            session_scope_id: session.session_scope_id().to_owned(),
            message_id: message.id.clone(),
            trust: ExternalTrust::ExternalUntrusted,
            sources: *external_sources,
            citations: Vec::new(),
        };
        controls.push(ControlEntry::ExternalProvenance(provenance));
    }
    if let Err(error) = session.append_tool_result_bundle(recorded, controls.clone()) {
        if !registrations.is_empty()
            && let Some(registrar) = registrar.as_ref()
        {
            let _ = registrar.rollback_message(&message.id);
        }
        return Err(error);
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
    for control in controls {
        handler.handle(RunEvent::Control(control))?;
    }
    result.content = display.preview.clone();
    result.metadata.bytes = Some(display.observed_bytes);
    result.metadata.returned_bytes = Some(display.preview.len() as u64);
    result.metadata.truncated = display.observed_bytes > display.preview.len() as u64;
    result.metadata.details = serde_json::to_value(&display)
        .unwrap_or_else(|_| serde_json::json!({"projection": "unavailable"}));
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
