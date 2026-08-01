use std::{collections::BTreeMap, path::Path, time::Instant};

use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::{
    ApprovalRequestIdentityV2, ControlEntry, ExecutionMutationProfile,
    MAX_DURABLE_PERMISSION_MATCHES, MAX_DURABLE_PERMISSION_REASONS,
    MAX_DURABLE_PERMISSION_TEXT_BYTES, MAX_DURABLE_TOOL_EXECUTION_DETAILS_BYTES,
    MAX_DURABLE_TOOL_EXECUTION_PATHS, MAX_DURABLE_TOOL_EXECUTION_RECEIPT_IDS,
    PreparedToolAuditBinding, RunEvent, Session, SessionLogEntry,
    TOOL_APPROVAL_AUDIT_SCHEMA_VERSION, TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION,
    TOOL_PERMISSION_DECISION_SCHEMA_VERSION, TerminalTaskEntry, ToolApprovalAllowSource,
    ToolApprovalAuditAction, ToolApprovalDecisionReceiptV2, ToolApprovalEntry,
    ToolApprovalSessionGrantEntry, ToolApprovalSessionGrantExpiry, ToolApprovalTerminalStatusV2,
    ToolApprovalUserDecision, ToolEgressAudit, ToolEgressEntry, ToolExecutionEntry,
    ToolExecutionStatus, ToolPermissionDecisionV2Entry, ToolPermissionPlanV2, ToolPreview,
    ToolResult, ToolResultMeta, ToolResultStatus, ToolSubjectAudit,
    event::EventHandler,
    permission::{
        PermissionDecision, tool_approval_session_grant_available_for_plan,
        tool_approval_session_grant_shape,
    },
    provider::ToolCall,
    time::saturating_elapsed,
    tool::{ToolSubject, ToolSubjectKind, ToolSubjectScope},
};

pub(super) fn has_external_subject(subjects: &[ToolSubject]) -> bool {
    subjects.iter().any(|subject| {
        subject.kind == ToolSubjectKind::Path && subject.scope == ToolSubjectScope::External
    })
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
        let command_hash = stable_text_hash(command);
        context.insert(
            "command_sha256".to_owned(),
            Value::String(command_hash.clone()),
        );
        summary_parts.push(format!("command_sha256={command_hash}"));
    }
    if let Some(path) = object
        .and_then(|object| object.get("path"))
        .and_then(Value::as_str)
    {
        let path_hash = stable_text_hash(path);
        context.insert("path_sha256".to_owned(), Value::String(path_hash.clone()));
        summary_parts.push(format!("path_sha256={path_hash}"));
    }
    if let Some(pattern) = object
        .and_then(|object| object.get("pattern"))
        .and_then(Value::as_str)
    {
        let pattern_hash = stable_text_hash(pattern);
        context.insert(
            "pattern_sha256".to_owned(),
            Value::String(pattern_hash.clone()),
        );
        summary_parts.push(format!("pattern_sha256={pattern_hash}"));
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
    format!(
        "{}:{}:{}",
        subject.scope.as_str(),
        subject.kind.as_str(),
        stable_text_hash(&subject.normalized)
    )
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
    identity: &ApprovalRequestIdentityV2,
    plan: &ToolPermissionPlanV2,
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
    reason: Option<String>,
    terminal_status: Option<ToolApprovalTerminalStatusV2>,
    preview_hash: Option<String>,
) -> Result<()> {
    append_tool_approval_audit_entry(
        session,
        call,
        decision,
        identity,
        plan,
        action,
        user_decision,
        reason,
        terminal_status,
        preview_hash,
    )
}

pub(super) fn append_tool_approval_policy_audit<H: EventHandler>(
    session: &mut Session,
    handler: &mut H,
    call: &ToolCall,
    decision: &PermissionDecision,
    plan: &ToolPermissionPlanV2,
    policy_version: &str,
    session_grant_source: Option<&ToolApprovalSessionGrantEntry>,
    prepared_digest: Option<String>,
) -> Result<()> {
    if call.id.trim().is_empty() || call.name != plan.tool_name || policy_version.trim().is_empty()
    {
        return Err(anyhow!("permission decision identity is incomplete"));
    }
    let control = ControlEntry::ToolPermissionDecisionV2(Box::new(ToolPermissionDecisionV2Entry {
        schema_version: TOOL_PERMISSION_DECISION_SCHEMA_VERSION,
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        plan_hash: plan.plan_hash.clone(),
        policy_version: policy_version.to_owned(),
        policy_decision: decision.mode,
        access: decision.access,
        network_effect: decision.network_effect,
        local_policy_decision: decision.local_policy_decision,
        network_policy_decision: decision.network_policy_decision,
        source_policy_decision: decision.source_policy_decision,
        operation: decision.operation,
        risk: decision.risk,
        subjects: audit_subjects(&decision.subjects),
        subject_zones: decision.subject_zones.clone(),
        external_directory_required: decision.external_directory_required,
        confirmation: audit_confirmation(decision.confirmation.as_ref()),
        snapshot_required: decision.snapshot_required,
        command_permission_matches: audit_command_matches(&decision.command_permission_matches)?,
        decision_reasons: audit_decision_reasons(&decision.reasons)?,
        allow_source: session_grant_source.map(|_| ToolApprovalAllowSource::SessionGrant),
        grant_id: session_grant_source.map(|grant| grant.grant_id.clone()),
        prepared_digest,
    }));
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

pub(super) fn append_tool_permission_plan_audit<H: EventHandler>(
    session: &mut Session,
    handler: &mut H,
    call: &ToolCall,
    plan: &crate::ToolPermissionPlanV2,
) -> Result<()> {
    let control = ControlEntry::ToolPermissionPlannedV2(Box::new(
        crate::ToolPermissionPlannedV2Entry::from_plan(&call.id, plan)?,
    ));
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
}

fn append_tool_approval_audit_entry(
    session: &mut Session,
    call: &ToolCall,
    decision: &PermissionDecision,
    identity: &ApprovalRequestIdentityV2,
    plan: &ToolPermissionPlanV2,
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
    reason: Option<String>,
    terminal_status: Option<ToolApprovalTerminalStatusV2>,
    preview_hash: Option<String>,
) -> Result<()> {
    validate_approval_audit_binding(call, identity, plan)?;
    let decision_receipt = (action == ToolApprovalAuditAction::DecisionAccepted)
        .then(|| {
            user_decision.map(|decision| ToolApprovalDecisionReceiptV2 {
                approval_request_id: identity.approval_request_id.clone(),
                decision,
                accepted_at_ms: super::unix_time_ms(),
            })
        })
        .flatten();
    let terminal_status = approval_terminal_status(action, user_decision, terminal_status);
    session.append_control(ControlEntry::ToolApproval(ToolApprovalEntry {
        schema_version: TOOL_APPROVAL_AUDIT_SCHEMA_VERSION,
        identity: identity.clone(),
        plan_hash: plan.plan_hash.clone(),
        action,
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        access: decision.access,
        network_effect: decision.network_effect,
        local_policy_decision: decision.local_policy_decision,
        network_policy_decision: decision.network_policy_decision,
        source_policy_decision: decision.source_policy_decision,
        operation: decision.operation,
        risk: decision.risk,
        subjects: audit_subjects(&decision.subjects),
        subject_zones: decision.subject_zones.clone(),
        policy_decision: decision.mode,
        external_directory_required: decision.external_directory_required,
        confirmation: audit_confirmation(decision.confirmation.as_ref()),
        snapshot_required: decision.snapshot_required,
        command_permission_matches: audit_command_matches(&decision.command_permission_matches)?,
        decision_reasons: audit_decision_reasons(&decision.reasons)?,
        user_decision,
        reason: reason.map(|value| bounded_safe_audit_text(&value)),
        preview_hash,
        decision_receipt,
        terminal_status,
    }))
}

fn validate_approval_audit_binding(
    call: &ToolCall,
    identity: &ApprovalRequestIdentityV2,
    plan: &ToolPermissionPlanV2,
) -> Result<()> {
    if identity.call_id != call.id
        || plan.tool_name != call.name
        || identity.plan_hash != plan.plan_hash
    {
        return Err(anyhow!(
            "approval audit identity changed before durable append"
        ));
    }
    Ok(())
}

fn approval_terminal_status(
    action: ToolApprovalAuditAction,
    user_decision: Option<ToolApprovalUserDecision>,
    route_terminal_status: Option<ToolApprovalTerminalStatusV2>,
) -> Option<ToolApprovalTerminalStatusV2> {
    match action {
        ToolApprovalAuditAction::PreviewFailed => None,
        ToolApprovalAuditAction::Resolved => match user_decision {
            Some(ToolApprovalUserDecision::Approved) => {
                Some(ToolApprovalTerminalStatusV2::Approved)
            }
            Some(ToolApprovalUserDecision::ApprovedForSession) => {
                Some(ToolApprovalTerminalStatusV2::ApprovedForSession)
            }
            Some(ToolApprovalUserDecision::Denied) => Some(ToolApprovalTerminalStatusV2::Denied),
            None => route_terminal_status,
        },
        ToolApprovalAuditAction::Requested | ToolApprovalAuditAction::DecisionAccepted => None,
    }
}

pub(super) fn append_tool_approval_session_grant<H: EventHandler>(
    session: &mut Session,
    handler: &mut H,
    call: &ToolCall,
    decision: &PermissionDecision,
    identity: &ApprovalRequestIdentityV2,
    plan: &ToolPermissionPlanV2,
) -> Result<()> {
    validate_approval_audit_binding(call, identity, plan)?;
    if !tool_approval_session_grant_available_for_plan(decision, plan) {
        return Err(anyhow!(
            "tool approval plan cannot be widened to a session grant"
        ));
    }
    let shape = tool_approval_session_grant_shape(decision)
        .ok_or_else(|| anyhow!("tool approval decision cannot be widened to a session grant"))?;
    let semantic_scope = plan
        .semantic_scope
        .clone()
        .ok_or_else(|| anyhow!("session grant requires a stable semantic scope"))?;
    let containment_binding = plan
        .session_grant_containment_binding()
        .ok_or_else(|| anyhow!("session grant requires a proven execution binding"))?;
    let control = ControlEntry::ToolApprovalSessionGrant(ToolApprovalSessionGrantEntry {
        schema_version: TOOL_APPROVAL_SESSION_GRANT_SCHEMA_VERSION,
        grant_id: uuid::Uuid::new_v4().to_string(),
        source_call_id: call.id.clone(),
        source_approval_request_id: identity.approval_request_id.clone(),
        tool_name: call.name.clone(),
        semantic_scope,
        effect_ceiling: plan.effects.clone(),
        risk_ceiling: decision.risk,
        subjects: audit_subjects(&decision.subjects),
        facets: shape.facets,
        scope: shape.scope,
        containment_binding,
        policy_version: identity.policy_version.clone(),
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
    let execution = durable_tool_execution_entry(call, subjects, status, duration_ms, result)?;
    session.append_control(ControlEntry::ToolExecution(Box::new(execution)))
}

/// Builds the canonical, bounded durable audit entry for one tool execution transition.
///
/// Host adapters that execute tools outside the main agent loop must use this projection instead
/// of persisting a raw [`ToolResultMeta`]. Tool-specific and display-only details are reduced to
/// the allowlisted durable schema, with omitted material represented by a content hash.
///
/// # Errors
///
/// Returns an error when result metadata cannot be safely projected into the durable contract.
pub fn durable_tool_execution_entry(
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    result: Option<&ToolResult>,
) -> Result<ToolExecutionEntry> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        let mut metadata = durable_tool_execution_metadata(&result.metadata)?;
        bind_canonical_tool_call_context(&mut metadata, &result.metadata, call, subjects)?;
        (
            durable_execution_path_labels(&result.metadata.changed_files),
            metadata,
            error.map(durable_tool_execution_error),
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

    Ok(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: audit_subjects(subjects),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })
}

fn bind_canonical_tool_call_context(
    projected: &mut ToolResultMeta,
    original: &ToolResultMeta,
    call: &ToolCall,
    subjects: &[ToolSubject],
) -> Result<()> {
    let canonical = tool_call_context(call, subjects);
    let original_call = original.details.get("call");
    let call_changed = original_call != canonical.as_ref();
    let details = projected
        .details
        .as_object_mut()
        .expect("durable metadata projection always returns an object");
    match canonical {
        Some(context) => {
            details.insert("call".to_owned(), context);
        }
        None => {
            details.remove("call");
        }
    }
    if call_changed {
        details.insert(
            "details_sha256".to_owned(),
            Value::String(stable_json_hash(&original.details)?),
        );
    }
    Ok(())
}

fn durable_tool_execution_metadata(metadata: &ToolResultMeta) -> Result<ToolResultMeta> {
    let mut projected = metadata.clone();
    projected.changed_files = durable_execution_path_labels(&metadata.changed_files);
    projected.details = durable_tool_execution_details(&metadata.details)?;
    if let Some(receipt) = &mut projected.receipt {
        receipt.idempotency_key = receipt.idempotency_key.as_deref().map(stable_text_hash);
        if receipt.mutation_operation_ids.len() > MAX_DURABLE_TOOL_EXECUTION_RECEIPT_IDS {
            return Err(anyhow!(
                "tool execution receipt ids exceed durable maximum of {}",
                MAX_DURABLE_TOOL_EXECUTION_RECEIPT_IDS
            ));
        }
        for operation_id in &mut receipt.mutation_operation_ids {
            *operation_id = bounded_safe_audit_text(operation_id);
        }
    }
    Ok(projected)
}

fn durable_execution_path_labels(paths: &[String]) -> Vec<String> {
    let retained = if paths.len() > MAX_DURABLE_TOOL_EXECUTION_PATHS {
        MAX_DURABLE_TOOL_EXECUTION_PATHS.saturating_sub(1)
    } else {
        MAX_DURABLE_TOOL_EXECUTION_PATHS
    };
    let mut labels = paths
        .iter()
        .take(retained)
        .map(|path| {
            let candidate = crate::safe_persistence_text(path);
            let parsed = Path::new(&candidate);
            let is_relative_safe = !candidate.is_empty()
                && candidate.len() <= crate::MAX_DURABLE_SUBJECT_LABEL_BYTES
                && !parsed.is_absolute()
                && !candidate.contains('\\')
                && candidate.as_bytes().get(1).is_none_or(|byte| *byte != b':')
                && !parsed.components().any(|component| {
                    matches!(
                        component,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
                && candidate == *path;
            if is_relative_safe {
                candidate
            } else {
                format!("sha256:{}", stable_text_hash(path))
            }
        })
        .collect::<Vec<_>>();
    if paths.len() > retained {
        let encoded = serde_json::to_string(paths).unwrap_or_else(|_| paths.join("\n"));
        labels.push(format!("sha256:{}", stable_text_hash(&encoded)));
    }
    labels
}

fn durable_tool_execution_details(details: &Value) -> Result<Value> {
    let mut projected = Map::new();
    let full_details_hash = stable_json_hash(details)?;
    let Some(object) = details.as_object() else {
        if !details.is_null() {
            projected.insert(
                "details_sha256".to_owned(),
                Value::String(stable_json_hash(details)?),
            );
        }
        return Ok(Value::Object(projected));
    };
    let mut omitted = Map::new();
    for (key, value) in object {
        if crate::session::DURABLE_TOOL_EXECUTION_DETAIL_KEYS.contains(&key.as_str())
            && key != "details_sha256"
        {
            let Some(safe) = durable_tool_execution_detail(key, value) else {
                continue;
            };
            if serde_json::to_vec(&safe)
                .context("failed to size tool execution detail")?
                .len()
                <= MAX_DURABLE_TOOL_EXECUTION_DETAILS_BYTES / 2
            {
                projected.insert(key.clone(), safe);
            } else {
                omitted.insert(key.clone(), value.clone());
            }
        } else {
            omitted.insert(key.clone(), value.clone());
        }
    }
    if !omitted.is_empty() {
        projected.insert(
            "details_sha256".to_owned(),
            Value::String(full_details_hash.clone()),
        );
    }
    let mut pruned = false;
    while serde_json::to_vec(&projected)
        .context("failed to size durable tool execution details")?
        .len()
        > MAX_DURABLE_TOOL_EXECUTION_DETAILS_BYTES
    {
        let Some(key) = projected
            .keys()
            .find(|key| key.as_str() != "details_sha256")
            .cloned()
        else {
            break;
        };
        projected.remove(&key);
        pruned = true;
    }
    if pruned {
        projected.insert(
            "details_sha256".to_owned(),
            Value::String(full_details_hash),
        );
    }
    Ok(Value::Object(projected))
}

fn durable_tool_execution_detail(key: &str, value: &Value) -> Option<Value> {
    if key != "output_hash" {
        return Some(durable_safe_json_value(value.clone()));
    }
    if value.is_null() {
        return None;
    }
    let Some(hash) = value.as_str() else {
        return Some(durable_safe_json_value(value.clone()));
    };
    let digest = hash.strip_prefix("sha256:").unwrap_or(hash);
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(Value::String(format!(
            "sha256:{}",
            digest.to_ascii_lowercase()
        )));
    }
    Some(durable_safe_json_value(value.clone()))
}

fn durable_safe_json_value(value: Value) -> Value {
    match crate::safe_persistence_json_value(value) {
        Value::String(value) => {
            let safe = bounded_safe_audit_text(&value);
            if durable_string_contains_absolute_path(&safe) {
                Value::String(format!("sha256:{}", stable_text_hash(&safe)))
            } else {
                Value::String(safe)
            }
        }
        Value::Array(values) => {
            Value::Array(values.into_iter().map(durable_safe_json_value).collect())
        }
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, durable_safe_json_value(value)))
                .collect(),
        ),
        value => value,
    }
}

fn durable_string_contains_absolute_path(value: &str) -> bool {
    value.split_whitespace().any(|token| {
        let token = token.trim_matches(|character: char| {
            matches!(character, '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';')
        });
        Path::new(token).is_absolute()
            || (token.as_bytes().get(1) == Some(&b':')
                && token
                    .as_bytes()
                    .get(2)
                    .is_some_and(|byte| matches!(byte, b'/' | b'\\')))
    })
}

fn durable_tool_execution_error(error: crate::ToolError) -> crate::ToolError {
    let details = if error.details.is_null() {
        Value::Null
    } else {
        serde_json::json!({
            "redacted": true,
            "sha256": stable_json_hash(&error.details)
                .unwrap_or_else(|_| stable_text_hash("unserializable-tool-error-details")),
        })
    };
    crate::ToolError {
        kind: error.kind,
        message: bounded_safe_audit_text(&error.message),
        retryable: error.retryable,
        details,
    }
}

pub(super) fn append_tool_execution_started_audit(
    session: &mut Session,
    handler: &mut impl EventHandler,
    call: &ToolCall,
    subjects: &[ToolSubject],
    permission_plan: Option<&ToolPermissionPlanV2>,
    approval_identity: Option<&ApprovalRequestIdentityV2>,
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
    if let Some(plan) = permission_plan {
        details.insert(
            "permission_plan_hash".to_owned(),
            Value::String(plan.plan_hash.clone()),
        );
        details.insert(
            "execution_containment".to_owned(),
            serde_json::to_value(&plan.containment)
                .context("failed to encode execution containment request")?,
        );
        if let Some(binding) = plan.session_grant_containment_binding() {
            details.insert(
                "execution_binding".to_owned(),
                serde_json::to_value(binding)
                    .context("failed to encode execution containment binding")?,
            );
        }
    }
    if let Some(identity) = approval_identity {
        details.insert(
            "approval_request_id".to_owned(),
            Value::String(identity.approval_request_id.clone()),
        );
    }
    if !details.is_empty() {
        metadata.details = Value::Object(details);
    }

    let control = ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status: ToolExecutionStatus::Started,
        duration_ms: None,
        subjects: audit_subjects(subjects),
        changed_files: Vec::new(),
        metadata,
        error: None,
        model_content_hash: None,
    }));
    session.append_control(control.clone())?;
    handler.handle(RunEvent::Control(control))
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
    let mut bundled_receipts = Vec::new();
    for control in std::mem::take(&mut result.control_entries) {
        if matches!(control, ControlEntry::ToolArtifactRead(_)) {
            bundled_receipts.push(control);
            continue;
        }
        session.append_control(control.clone())?;
        handler.handle(RunEvent::Control(control))?;
    }
    result.control_entries = bundled_receipts;
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

fn audit_confirmation(
    confirmation: Option<&crate::PermissionConfirmation>,
) -> Option<crate::PermissionConfirmation> {
    confirmation.map(|confirmation| match confirmation {
        crate::PermissionConfirmation::Standard => crate::PermissionConfirmation::Standard,
        crate::PermissionConfirmation::TypePath => crate::PermissionConfirmation::TypePath,
        crate::PermissionConfirmation::TypePhrase { phrase } => {
            crate::PermissionConfirmation::TypePhrase {
                phrase: stable_text_hash(phrase),
            }
        }
    })
}

fn audit_command_matches(
    matches: &[crate::CommandPermissionMatch],
) -> Result<Vec<crate::CommandPermissionMatch>> {
    if matches.len() > MAX_DURABLE_PERMISSION_MATCHES {
        return Err(anyhow!(
            "permission decision command matches exceed durable maximum of {}",
            MAX_DURABLE_PERMISSION_MATCHES
        ));
    }
    Ok(matches
        .iter()
        .map(|matched| crate::CommandPermissionMatch {
            group: matched.group,
            pattern: stable_text_hash(&matched.pattern),
            command: stable_text_hash(&matched.command),
        })
        .collect())
}

fn audit_decision_reasons(
    reasons: &[crate::PermissionDecisionReason],
) -> Result<Vec<crate::PermissionDecisionReason>> {
    if reasons.len() > MAX_DURABLE_PERMISSION_REASONS {
        return Err(anyhow!(
            "permission decision reasons exceed durable maximum of {}",
            MAX_DURABLE_PERMISSION_REASONS
        ));
    }
    Ok(reasons
        .iter()
        .map(|reason| crate::PermissionDecisionReason {
            source: reason.source,
            code: bounded_safe_audit_text(&reason.code),
            detail: bounded_safe_audit_text(&reason.detail),
        })
        .collect())
}

fn bounded_safe_audit_text(value: &str) -> String {
    let safe = crate::safe_persistence_text(value)
        .split_whitespace()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            if lower.contains("http://") || lower.contains("https://") {
                "[redacted-url]"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    if safe.len() <= MAX_DURABLE_PERMISSION_TEXT_BYTES {
        return safe;
    }
    let mut boundary = MAX_DURABLE_PERMISSION_TEXT_BYTES;
    while boundary > 0 && !safe.is_char_boundary(boundary) {
        boundary -= 1;
    }
    safe[..boundary].to_owned()
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
