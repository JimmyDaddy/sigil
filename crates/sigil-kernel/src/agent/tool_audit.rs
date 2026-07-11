use std::{collections::BTreeMap, path::Path, time::Instant};

use anyhow::{Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    ControlEntry, ExecutionMutationProfile, PreparedToolAuditBinding, RunEvent, Session,
    SessionLogEntry, TerminalTaskEntry, ToolApprovalAllowSource, ToolApprovalAuditAction,
    ToolApprovalEntry, ToolApprovalSessionGrantEntry, ToolApprovalSessionGrantExpiry,
    ToolApprovalUserDecision, ToolEgressAudit, ToolEgressEntry, ToolExecutionEntry,
    ToolExecutionStatus, ToolPreview, ToolResult, ToolResultMeta, ToolResultStatus,
    ToolSubjectAudit,
    event::EventHandler,
    permission::PermissionDecision,
    provider::ToolCall,
    time::saturating_elapsed,
    tool::{ToolSubject, ToolSubjectScope},
};

pub(super) fn has_external_subject(subjects: &[ToolSubject]) -> bool {
    subjects
        .iter()
        .any(|subject| subject.scope == ToolSubjectScope::External)
}

pub(super) fn attach_tool_call_context(
    result: &mut ToolResult,
    call: &ToolCall,
    subjects: &[ToolSubject],
) {
    let Some(context) = tool_call_context(call, subjects) else {
        return;
    };
    match &mut result.metadata.details {
        Value::Object(details) => {
            details.insert("call".to_owned(), context);
        }
        Value::Null => {
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            result.metadata.details = Value::Object(details);
        }
        existing => {
            let previous = std::mem::replace(existing, Value::Null);
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            details.insert("tool".to_owned(), previous);
            *existing = Value::Object(details);
        }
    }
}

pub(super) fn attach_prepared_tool_audit_binding(
    result: &mut ToolResult,
    binding: &PreparedToolAuditBinding,
) -> Result<()> {
    let value = serde_json::to_value(binding)
        .context("failed to encode prepared mutation audit binding")?;
    match &mut result.metadata.details {
        Value::Object(details) => {
            details.insert("prepared_mutation".to_owned(), value);
        }
        Value::Null => {
            let mut details = Map::new();
            details.insert("prepared_mutation".to_owned(), value);
            result.metadata.details = Value::Object(details);
        }
        existing => {
            let previous = std::mem::replace(existing, Value::Null);
            let mut details = Map::new();
            details.insert("prepared_mutation".to_owned(), value);
            details.insert("tool".to_owned(), previous);
            *existing = Value::Object(details);
        }
    }
    Ok(())
}

pub(super) fn tool_call_context(call: &ToolCall, subjects: &[ToolSubject]) -> Option<Value> {
    let args = serde_json::from_str::<Value>(&call.args_json).ok();
    let object = args.as_ref().and_then(Value::as_object);
    let mut context = Map::new();
    let mut summary_parts = Vec::new();

    if let Some(command) = object
        .and_then(|object| object.get("command"))
        .and_then(Value::as_str)
    {
        let command = truncate_context_value(command);
        context.insert("command".to_owned(), Value::String(command.clone()));
        summary_parts.push(format!("command={command}"));
    }
    if let Some(path) = object
        .and_then(|object| object.get("path"))
        .and_then(Value::as_str)
    {
        let path = truncate_context_value(path);
        context.insert("path".to_owned(), Value::String(path.clone()));
        summary_parts.push(format!("path={path}"));
    }
    if let Some(pattern) = object
        .and_then(|object| object.get("pattern"))
        .and_then(Value::as_str)
    {
        let pattern = truncate_context_value(pattern);
        context.insert("pattern".to_owned(), Value::String(pattern.clone()));
        summary_parts.push(format!("pattern={pattern}"));
    }

    let subject_labels = subjects
        .iter()
        .take(6)
        .map(tool_subject_context_label)
        .collect::<Vec<_>>();
    if !subject_labels.is_empty() {
        context.insert(
            "subjects".to_owned(),
            Value::Array(subject_labels.iter().cloned().map(Value::String).collect()),
        );
        if summary_parts.is_empty() {
            summary_parts.push(format!("subject={}", subject_labels.join(",")));
        }
    }

    if !summary_parts.is_empty() {
        context.insert(
            "summary".to_owned(),
            Value::String(truncate_context_value(&summary_parts.join(" "))),
        );
    }

    (!context.is_empty()).then_some(Value::Object(context))
}

fn tool_subject_context_label(subject: &ToolSubject) -> String {
    let target = subject
        .canonical_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| subject.normalized.clone());
    truncate_context_value(&format!(
        "{}:{}:{}",
        subject.scope.as_str(),
        subject.kind.as_str(),
        target
    ))
}

fn truncate_context_value(value: &str) -> String {
    const MAX_CHARS: usize = 180;
    let normalized = crate::safe_persistence_text(value);
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

pub(super) fn external_directory_preview(tool_name: &str, subjects: &[ToolSubject]) -> ToolPreview {
    let external_subjects = subjects
        .iter()
        .filter(|subject| subject.scope == ToolSubjectScope::External)
        .map(|subject| {
            subject
                .canonical_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| subject.normalized.clone())
        })
        .collect::<Vec<_>>();
    let body = if external_subjects.is_empty() {
        "No external path subjects were reported.".to_owned()
    } else {
        external_subjects.join("\n")
    };
    ToolPreview {
        title: format!("External directory access for {tool_name}"),
        summary: "This tool call touches a path outside the workspace.".to_owned(),
        body,
        changed_files: Vec::new(),
        file_diffs: Vec::new(),
    }
}

pub(super) fn append_tool_approval_audit(
    session: &mut Session,
    call: &ToolCall,
    decision: &PermissionDecision,
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
    reason: Option<String>,
    preview_hash: Option<String>,
) -> Result<()> {
    append_tool_approval_audit_with_source(
        session,
        call,
        decision,
        action,
        None,
        None,
        user_decision,
        reason,
        preview_hash,
    )
}

pub(super) fn append_tool_approval_policy_audit(
    session: &mut Session,
    call: &ToolCall,
    decision: &PermissionDecision,
    session_grant_source: Option<&ToolApprovalSessionGrantEntry>,
    prepared_digest: Option<String>,
) -> Result<()> {
    append_tool_approval_audit_with_source(
        session,
        call,
        decision,
        ToolApprovalAuditAction::PolicyEvaluated,
        session_grant_source.map(|_| ToolApprovalAllowSource::SessionGrant),
        session_grant_source.map(|grant| grant.call_id.clone()),
        None,
        None,
        prepared_digest,
    )
}

fn append_tool_approval_audit_with_source(
    session: &mut Session,
    call: &ToolCall,
    decision: &PermissionDecision,
    action: ToolApprovalAuditAction,
    allow_source: Option<ToolApprovalAllowSource>,
    grant_call_id: Option<String>,
    user_decision: Option<ToolApprovalUserDecision>,
    reason: Option<String>,
    preview_hash: Option<String>,
) -> Result<()> {
    session.append_control(ControlEntry::ToolApproval(ToolApprovalEntry {
        action,
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        access: decision.access,
        network_effect: decision.network_effect,
        local_policy_decision: decision.local_policy_decision,
        network_policy_decision: decision.network_policy_decision,
        source_policy_decision: decision.source_policy_decision,
        operation: Some(decision.operation),
        risk: Some(decision.risk),
        subjects: audit_subjects(&decision.subjects),
        subject_zones: decision.subject_zones.clone(),
        policy_decision: decision.mode,
        external_directory_required: decision.external_directory_required,
        confirmation: decision.confirmation.clone(),
        snapshot_required: decision.snapshot_required,
        command_permission_matches: decision.command_permission_matches.clone(),
        allow_source,
        grant_call_id,
        user_decision,
        reason,
        preview_hash,
    }))
}

pub(super) fn append_tool_approval_session_grant<H: EventHandler>(
    session: &mut Session,
    handler: &mut H,
    call: &ToolCall,
    decision: &PermissionDecision,
) -> Result<()> {
    let control = ControlEntry::ToolApprovalSessionGrant(ToolApprovalSessionGrantEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        access: decision.access,
        network_effect: decision.network_effect,
        operation: decision.operation,
        risk: decision.risk,
        subjects: audit_subjects(&decision.subjects),
        subject_zones: decision.subject_zones.clone(),
        expires: ToolApprovalSessionGrantExpiry::Session,
        granted_at_ms: super::unix_time_ms(),
    });
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

pub(super) fn append_tool_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    result: Option<&ToolResult>,
) -> Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            result.metadata.changed_files.clone(),
            result.metadata.clone(),
            error,
            Some(stable_text_hash(&result.to_model_content())),
        )
    } else {
        let mut metadata = ToolResultMeta::default();
        if let Some(context) = tool_call_context(call, subjects) {
            let mut details = Map::new();
            details.insert("call".to_owned(), context);
            metadata.details = Value::Object(details);
        }
        (Vec::new(), metadata, None, None)
    };

    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: audit_subjects(subjects),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })))
}

pub(super) fn append_tool_execution_started_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    execution_mutation_profile: Option<&ExecutionMutationProfile>,
    prepared_binding: Option<&PreparedToolAuditBinding>,
) -> Result<()> {
    let mut metadata = ToolResultMeta::default();
    let mut details = Map::new();
    if let Some(context) = tool_call_context(call, subjects) {
        details.insert("call".to_owned(), context);
    }
    if let Some(profile) = execution_mutation_profile {
        details.insert(
            "execution_mutation_profile".to_owned(),
            serde_json::to_value(profile).context("failed to encode execution mutation profile")?,
        );
    }
    if let Some(binding) = prepared_binding {
        details.insert(
            "prepared_mutation".to_owned(),
            serde_json::to_value(binding)
                .context("failed to encode prepared mutation audit binding")?,
        );
    }
    if !details.is_empty() {
        metadata.details = Value::Object(details);
    }

    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status: ToolExecutionStatus::Started,
        duration_ms: None,
        subjects: audit_subjects(subjects),
        changed_files: Vec::new(),
        metadata,
        error: None,
        model_content_hash: None,
    })))
}

pub(super) fn append_terminal_task_control_from_result(
    session: &mut Session,
    handler: &mut impl EventHandler,
    result: &ToolResult,
) -> Result<Option<TerminalTaskEntry>> {
    let Some(entry) = TerminalTaskEntry::from_tool_result_details(&result.metadata.details)? else {
        return Ok(None);
    };
    let control = ControlEntry::TerminalTask(entry.clone());
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))?;
    Ok(Some(entry))
}

pub(super) fn reconcile_terminal_task_mutation_from_start(
    session: &Session,
    workspace_root: &Path,
    entry: &TerminalTaskEntry,
) -> Result<()> {
    if !entry.status.is_terminal() {
        return Ok(());
    }
    let Some(profile) =
        terminal_start_execution_profile_for_task(session.entries(), &entry.handle.task_id)
    else {
        return Ok(());
    };
    let Some(recorder) = session.mutation_event_recorder() else {
        return Ok(());
    };
    recorder.reconcile_execution_mutation_profile(workspace_root, &profile)?;
    Ok(())
}

fn terminal_start_execution_profile_for_task(
    entries: &[SessionLogEntry],
    task_id: &crate::TerminalTaskId,
) -> Option<ExecutionMutationProfile> {
    let mut profiles = BTreeMap::<String, ExecutionMutationProfile>::new();
    for entry in entries {
        let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
            continue;
        };
        if execution.tool_name != "terminal_start" {
            continue;
        }
        if execution.status == ToolExecutionStatus::Started
            && let Some(profile) = execution_mutation_profile_from_details(&execution.metadata)
        {
            profiles.insert(execution.call_id.clone(), profile);
            continue;
        }
        if terminal_task_id_from_tool_metadata(&execution.metadata)
            .as_deref()
            .is_some_and(|recorded| recorded == task_id.as_str())
            && let Some(profile) = profiles.get(&execution.call_id)
        {
            return Some(profile.clone());
        }
    }
    None
}

pub(super) fn execution_mutation_profile_from_details(
    metadata: &ToolResultMeta,
) -> Option<ExecutionMutationProfile> {
    metadata
        .details
        .get("execution_mutation_profile")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(super) fn terminal_task_id_from_tool_metadata(metadata: &ToolResultMeta) -> Option<String> {
    metadata
        .details
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

pub(super) fn append_tool_control_entries_from_result(
    session: &mut Session,
    handler: &mut impl EventHandler,
    result: &mut ToolResult,
) -> Result<()> {
    for control in std::mem::take(&mut result.control_entries) {
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    Ok(())
}

pub(super) fn tool_egress_control_entry(
    call: &ToolCall,
    subjects: &[ToolSubject],
    audit: ToolEgressAudit,
) -> ControlEntry {
    ControlEntry::ToolEgress(Box::new(ToolEgressEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        destination: audit.destination,
        operation: audit.operation,
        subjects: audit_subjects(subjects),
        payload: audit.payload,
        redacted: audit.redacted,
    }))
}

fn audit_subjects(subjects: &[ToolSubject]) -> Vec<ToolSubjectAudit> {
    subjects.iter().map(ToolSubjectAudit::from).collect()
}

pub(super) fn stable_json_hash<T: serde::Serialize>(value: &T) -> Result<String> {
    let serialized = serde_json::to_string(value).context("failed to serialize audit payload")?;
    Ok(stable_text_hash(&serialized))
}

pub(super) fn stable_text_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

pub(super) fn duration_ms(started_at: Instant) -> u64 {
    saturating_elapsed(started_at)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
