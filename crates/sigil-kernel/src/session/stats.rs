use super::*;
use crate::ResolvedModelRoute;

pub(super) fn apply_usage_control_entry(stats: &mut SessionStats, control: &ControlEntry) {
    match control {
        ControlEntry::UsageSnapshot(usage) => stats.apply_usage(usage),
        ControlEntry::SemanticCompactionUsageSnapshot(usage) => {
            stats.apply_semantic_compaction_usage(usage);
        }
        _ => {}
    }
}

pub(super) fn repair_orphan_tool_results(messages: &[ModelMessage]) -> Vec<ModelMessage> {
    let mut repaired = Vec::with_capacity(messages.len());
    let mut index = 0usize;

    while index < messages.len() {
        let message = &messages[index];
        repaired.push(message.clone());

        if !matches!(message.role, crate::MessageRole::Assistant) || message.tool_calls.is_empty() {
            index += 1;
            continue;
        }

        index += 1;
        let mut satisfied_call_ids = Vec::new();
        while index < messages.len() && matches!(messages[index].role, crate::MessageRole::Tool) {
            if let Some(tool_call_id) = &messages[index].tool_call_id
                && message
                    .tool_calls
                    .iter()
                    .any(|call| call.id == *tool_call_id)
            {
                satisfied_call_ids.push(tool_call_id.clone());
            }
            repaired.push(messages[index].clone());
            index += 1;
        }

        for call in &message.tool_calls {
            if !satisfied_call_ids.iter().any(|call_id| call_id == &call.id) {
                repaired.push(synthetic_orphan_tool_result(call));
            }
        }
    }

    repaired
}

pub(super) fn synthetic_orphan_tool_result(call: &crate::ToolCall) -> ModelMessage {
    let result = ToolResult::error(
        call.id.clone(),
        call.name.clone(),
        ToolErrorKind::Interrupted,
        format!(
            "tool call {} did not return a result before the previous run stopped; retry the tool call with valid arguments if it is still needed",
            call.name
        ),
    );
    let mut message = result.to_model_message();
    message.id = format!("local_repair:missing_tool_result:{}", call.id);
    message
}

pub(super) fn interrupted_tool_executions(entries: &[SessionLogEntry]) -> Vec<ToolExecutionEntry> {
    let mut open_executions = HashMap::<String, ToolExecutionEntry>::new();
    for entry in entries {
        let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
            continue;
        };
        match execution.status {
            ToolExecutionStatus::Started => {
                open_executions.insert(execution.call_id.clone(), execution.as_ref().clone());
            }
            ToolExecutionStatus::Completed
            | ToolExecutionStatus::Failed
            | ToolExecutionStatus::Cancelled
            | ToolExecutionStatus::Interrupted => {
                open_executions.remove(&execution.call_id);
            }
        }
    }

    open_executions
        .into_values()
        .map(|mut execution| {
            execution.status = ToolExecutionStatus::Interrupted;
            execution.duration_ms = None;
            execution.changed_files = Vec::new();
            execution.metadata.changed_files = Vec::new();
            execution.error = Some(ToolError {
                kind: ToolErrorKind::Interrupted,
                message: "tool execution was interrupted before a completion record was written"
                    .to_owned(),
                retryable: true,
                details: serde_json::Value::Null,
            });
            execution.model_content_hash = None;
            execution
        })
        .collect()
}

pub(super) fn unresolved_tool_approvals(
    entries: &[SessionLogEntry],
    recovered_at_ms: u64,
) -> Vec<ToolApprovalEntry> {
    let mut open = BTreeMap::<String, (ToolApprovalEntry, Option<ToolApprovalUserDecision>)>::new();
    let mut started_calls = BTreeMap::<String, ()>::new();

    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::ToolApproval(approval)) => {
                let request_id = approval.identity.approval_request_id.clone();
                match approval.action {
                    ToolApprovalAuditAction::Requested => {
                        open.insert(request_id, (approval.clone(), None));
                    }
                    ToolApprovalAuditAction::DecisionAccepted => {
                        if let Some(pending) = open.get_mut(&request_id) {
                            pending.0 = approval.clone();
                            pending.1 = approval.user_decision;
                        }
                    }
                    ToolApprovalAuditAction::Resolved => {
                        open.remove(&request_id);
                    }
                    ToolApprovalAuditAction::PreviewFailed => {}
                }
            }
            SessionLogEntry::Control(ControlEntry::ToolExecution(execution))
                if execution.status == ToolExecutionStatus::Started =>
            {
                started_calls.insert(execution.call_id.clone(), ());
            }
            _ => {}
        }
    }

    open
        .into_values()
        .map(|(mut approval, accepted)| {
            let execution_started = started_calls.contains_key(&approval.call_id);
            let (user_decision, terminal_status, reason) = match accepted {
                Some(ToolApprovalUserDecision::Denied) => (
                    Some(ToolApprovalUserDecision::Denied),
                    ToolApprovalTerminalStatusV2::Denied,
                    "accepted denial was reconciled after session restart",
                ),
                Some(ToolApprovalUserDecision::Approved) if execution_started => (
                    Some(ToolApprovalUserDecision::Approved),
                    ToolApprovalTerminalStatusV2::Approved,
                    "accepted approval was reconciled from execution-start evidence",
                ),
                Some(ToolApprovalUserDecision::ApprovedForSession) if execution_started => (
                    Some(ToolApprovalUserDecision::ApprovedForSession),
                    ToolApprovalTerminalStatusV2::ApprovedForSession,
                    "accepted session approval was reconciled from execution-start evidence",
                ),
                Some(ToolApprovalUserDecision::Approved)
                | Some(ToolApprovalUserDecision::ApprovedForSession) => (
                    None,
                    ToolApprovalTerminalStatusV2::Stale,
                    "accepted approval was invalidated before execution started during session restart",
                ),
                None if recovered_at_ms >= approval.identity.expires_at_ms => (
                    None,
                    ToolApprovalTerminalStatusV2::Expired,
                    "approval request expired before session recovery",
                ),
                None => (
                    None,
                    ToolApprovalTerminalStatusV2::Cancelled,
                    "approval request was interrupted by session restart",
                ),
            };
            approval.action = ToolApprovalAuditAction::Resolved;
            approval.user_decision = user_decision;
            approval.reason = Some(reason.to_owned());
            approval.decision_receipt = None;
            approval.terminal_status = Some(terminal_status);
            approval
        })
        .collect()
}

pub(super) fn interrupted_tool_execution_profiles(
    entries: &[SessionLogEntry],
) -> Vec<ExecutionMutationProfile> {
    entries
        .iter()
        .filter_map(|entry| {
            let SessionLogEntry::Control(ControlEntry::ToolExecution(execution)) = entry else {
                return None;
            };
            if execution.status != ToolExecutionStatus::Interrupted {
                return None;
            }
            execution
                .metadata
                .details
                .get("execution_mutation_profile")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok())
        })
        .collect()
}

pub fn session_stats_from_entries(entries: &[SessionLogEntry]) -> SessionStats {
    let mut stats = SessionStats::default();
    for entry in entries {
        match entry {
            SessionLogEntry::Control(control) => apply_usage_control_entry(&mut stats, control),
            SessionLogEntry::User(_)
            | SessionLogEntry::Assistant(_)
            | SessionLogEntry::RuntimeContextSnapshotV2(_)
            | SessionLogEntry::ToolResultV3(_) => {}
        }
    }
    stats
}

pub(super) fn stable_json_hash(value: &serde_json::Value) -> String {
    let serialized =
        serde_json::to_string(value).unwrap_or_else(|_| "<unserializable-json>".to_owned());
    stable_text_hash(&serialized)
}

pub(super) fn stable_text_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    format!("{digest:x}")
}

pub(super) fn json_object_keys(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(object) = value.and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

pub(super) fn json_string_array(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(values) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut strings = values
        .iter()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    strings.sort();
    strings
}

pub(super) fn json_top_level_keys(value: &serde_json::Value) -> Vec<String> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    let mut keys = object.keys().cloned().collect::<Vec<_>>();
    keys.sort();
    keys
}

pub(super) fn session_identity_from_entries(
    entries: &[SessionLogEntry],
) -> Option<(String, String)> {
    let mut identity = None;
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name,
                model_name,
                ..
            }) if identity.is_none() => {
                identity = Some((provider_name.clone(), model_name.clone()));
            }
            SessionLogEntry::Control(
                ControlEntry::SessionModelSelected {
                    provider_name,
                    model_name,
                    ..
                }
                | ControlEntry::SessionRouteRebound {
                    provider_name,
                    model_name,
                    ..
                },
            ) if identity.is_some() => {
                if let Some((current_provider, current_model)) = identity.as_mut() {
                    *current_provider = provider_name.clone();
                    *current_model = model_name.clone();
                }
            }
            _ => {}
        }
    }
    identity
}

pub(super) fn session_resolved_route_from_entries(
    entries: &[SessionLogEntry],
) -> Option<ResolvedModelRoute> {
    let mut route = None;
    let mut identity_seen = false;
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route,
                ..
            }) if !identity_seen => {
                identity_seen = true;
                route = resolved_model_route.clone();
            }
            SessionLogEntry::Control(
                ControlEntry::SessionModelSelected {
                    resolved_model_route,
                    ..
                }
                | ControlEntry::SessionRouteRebound {
                    resolved_model_route,
                    ..
                },
            ) if identity_seen => route = Some(resolved_model_route.clone()),
            _ => {}
        }
    }
    let route = route?;
    let (_, final_model) = session_identity_from_entries(entries)?;
    (route.model_ref.model_id == final_model).then_some(route)
}

pub(super) fn session_route_trust_binding_from_entries(
    entries: &[SessionLogEntry],
) -> Option<crate::RouteEgressTrustBinding> {
    let mut current_fingerprint = None::<String>;
    let mut trust_binding = None;
    let mut identity_seen = false;
    for entry in entries {
        match entry {
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                resolved_model_route,
                ..
            }) if !identity_seen => {
                identity_seen = true;
                current_fingerprint = resolved_model_route
                    .as_ref()
                    .map(|route| route.semantic_fingerprint.clone());
                trust_binding = None;
            }
            SessionLogEntry::Control(
                ControlEntry::SessionModelSelected {
                    resolved_model_route,
                    ..
                }
                | ControlEntry::SessionRouteRebound {
                    resolved_model_route,
                    ..
                },
            ) if identity_seen => {
                current_fingerprint = Some(resolved_model_route.semantic_fingerprint.clone());
                trust_binding = None;
            }
            SessionLogEntry::Control(ControlEntry::SessionRouteTrustBound {
                route_semantic_fingerprint,
                egress_trust_binding,
            }) if current_fingerprint.as_deref() == Some(route_semantic_fingerprint.as_str()) => {
                trust_binding = Some(egress_trust_binding.clone());
            }
            _ => {}
        }
    }
    trust_binding
}
