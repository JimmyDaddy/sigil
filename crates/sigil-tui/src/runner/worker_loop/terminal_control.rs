use super::*;

pub(in crate::runner) fn cancel_terminal_task(
    runtime: &tokio::runtime::Runtime,
    registry: ToolRegistry,
    terminal_control: &sigil_tools_builtin::TerminalTaskControlHandle,
    root_config: &RootConfig,
    options: &AgentRunOptions,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    identity: &TerminalTaskControlIdentity,
) -> std::result::Result<(TerminalTaskEntry, Vec<SessionLogEntry>), String> {
    let mut session = load_session_with_runtime_attachments(
        &root_config.agent.runtime_provider,
        &root_config.agent.model,
        current_session_log_path,
        current_session.as_ref(),
    )
    .map_err(|error| format!("failed to load session before terminal cancel: {error:#}"))?;
    if session.session_scope_id() != identity.session_scope_id {
        return Err("terminal cancel session owner changed".to_owned());
    }
    let task_id = identity.task_id.clone();
    let terminal_task_id = TerminalTaskId::new(task_id.clone())
        .map_err(|error| format!("invalid terminal task id: {error:#}"))?;
    let projection = session.terminal_task_projection();
    let previous = projection
        .tasks
        .get(&terminal_task_id)
        .cloned()
        .ok_or_else(|| format!("terminal task {task_id} is not in the current session"))?;
    if !previous.status.is_active() {
        return Err(format!("terminal task {task_id} is not running"));
    }
    if previous.generation != identity.expected_generation {
        return Err(format!(
            "terminal task {task_id} generation changed from {} to {}",
            identity.expected_generation, previous.generation
        ));
    }

    let terminal_mutation_profile =
        terminal_start_execution_profile_for_task(session.entries(), &terminal_task_id);
    let mutation_recorder = MutationEventRecorder::new(
        JsonlSessionStore::new(current_session_log_path)
            .map_err(|error| format!("failed to open mutation recorder: {error:#}"))?,
    );
    let tool_context = ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs)
        .with_mutation_recorder(mutation_recorder.clone());
    let call = ToolCall {
        id: format!("tui-terminal-cancel-{task_id}"),
        name: "terminal_cancel".to_owned(),
        args_json: serde_json::json!({ "task_id": task_id }).to_string(),
    };
    let permission_plan = registry
        .permission_plan(&tool_context, &call)
        .map_err(|error| format!("invalid terminal cancel arguments: {error:#}"))?;
    let subjects = permission_plan.subjects.clone();
    session
        .append_control(ControlEntry::ToolPermissionPlannedV2(Box::new(
            sigil_kernel::ToolPermissionPlannedV2Entry::from_plan(&call.id, &permission_plan)
                .map_err(|error| {
                    format!("failed to project terminal cancel permission plan: {error:#}")
                })?,
        )))
        .map_err(|error| format!("failed to append terminal cancel permission plan: {error:#}"))?;
    let cancel_mutation_profile = registry
        .execution_mutation_profile(&tool_context, &call)
        .map_err(|error| {
            format!("failed to capture terminal cancel mutation profile: {error:#}")
        })?;
    append_terminal_cancel_execution_audit(
        &mut session,
        &call,
        &subjects,
        ToolExecutionStatus::Started,
        None,
        Some(&permission_plan),
        cancel_mutation_profile.as_ref(),
        None,
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;

    let execution_started = Instant::now();
    let owner_result = runtime.block_on(async {
        let before = terminal_control
            .status(&options.workspace_root, &terminal_task_id)
            .await?;
        if before.generation != identity.expected_generation {
            anyhow::bail!(
                "live terminal generation changed from {} to {}",
                identity.expected_generation,
                before.generation
            );
        }
        terminal_control
            .cancel(&options.workspace_root, &terminal_task_id)
            .await
    });
    let result = match owner_result {
        Ok(entry) => terminal_cancel_result(&call, &entry),
        Err(error) => ToolResult::error(
            call.id.clone(),
            call.name.clone(),
            ToolErrorKind::Internal,
            format!("terminal cancel failed: {error:#}"),
        ),
    };
    let duration_ms = Some(elapsed_ms(execution_started));
    let execution_status = if result.is_error() {
        ToolExecutionStatus::Failed
    } else {
        ToolExecutionStatus::Completed
    };
    append_terminal_cancel_execution_audit(
        &mut session,
        &call,
        &subjects,
        execution_status,
        duration_ms,
        Some(&permission_plan),
        None,
        Some(&result),
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;
    if result.is_error() {
        *current_session = Some(session);
        return Err(format!("terminal cancel failed: {}", result.content));
    }
    let entry = terminal_cancel_entry_from_result(&previous, &result)?;
    session
        .append_control(ControlEntry::TerminalTask(entry.clone()))
        .map_err(|error| format!("failed to append terminal task state: {error:#}"))?;
    if let Some(profile) = terminal_mutation_profile {
        mutation_recorder
            .reconcile_execution_mutation_profile(&options.workspace_root, &profile)
            .map_err(|error| {
                format!("failed to reconcile terminal task workspace mutation: {error:#}")
            })?;
    }
    let entries = session.entries().to_vec();
    *current_session = Some(session);
    Ok((entry, entries))
}

fn terminal_cancel_result(call: &ToolCall, entry: &TerminalTaskEntry) -> ToolResult {
    ToolResult::ok(
        call.id.clone(),
        call.name.clone(),
        format!(
            "cancelled terminal task {}\nstatus: {}\nlog: {}",
            entry.handle.task_id.as_str(),
            entry.status.as_str(),
            entry.handle.log_ref
        ),
        ToolResultMeta {
            truncated: entry.output_truncated,
            total_bytes: Some(entry.output_total_bytes),
            limit_bytes: entry.output_limit_bytes,
            details: serde_json::json!({
                "schema_version": entry.schema_version,
                "task_id": entry.handle.task_id.as_str(),
                "generation": entry.generation,
                "status": entry.status.as_str(),
                "status_detail": &entry.status,
                "readiness": &entry.readiness,
                "command_sha256": &entry.handle.command_sha256,
                "cwd_label": &entry.handle.cwd_label,
                "shell_label": &entry.handle.shell_label,
                "shell_sha256": &entry.handle.shell_sha256,
                "log_ref": &entry.handle.log_ref,
                "created_at_ms": entry.handle.created_at_ms,
                "updated_at_ms": entry.updated_at_ms,
                "execution_backend": &entry.handle.execution_backend,
                "execution_backend_capabilities": &entry.handle.execution_backend_capabilities,
                "enforcement_backend": &entry.handle.enforcement_backend,
                "enforcement_backend_capabilities": &entry.handle.enforcement_backend_capabilities,
                "sandbox_profile": &entry.handle.sandbox_profile,
                "output_preview": &entry.output_preview,
                "output_hash": &entry.output_hash,
                "output_truncated": entry.output_truncated,
                "output_total_bytes": entry.output_total_bytes,
                "output_limit_bytes": entry.output_limit_bytes,
                "output_termination_reason": &entry.output_termination_reason,
                "cleanup": &entry.cleanup
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(in crate::runner) fn terminal_cancel_entry_from_result(
    previous: &sigil_kernel::TerminalTaskSummary,
    result: &ToolResult,
) -> std::result::Result<TerminalTaskEntry, String> {
    let entry = TerminalTaskEntry::from_tool_result_details(&result.metadata.details)
        .map_err(|error| format!("invalid terminal cancel result: {error:#}"))?
        .ok_or_else(|| "terminal cancel result did not include terminal task state".to_owned())?;
    if entry.handle.task_id != previous.handle.task_id {
        return Err(format!(
            "terminal cancel returned task {}, expected {}",
            entry.handle.task_id.as_str(),
            previous.handle.task_id.as_str()
        ));
    }
    Ok(entry)
}

pub(in crate::runner) fn terminal_start_execution_profile_for_task(
    entries: &[SessionLogEntry],
    task_id: &TerminalTaskId,
) -> Option<ExecutionMutationProfile> {
    let mut profiles = std::collections::BTreeMap::<String, ExecutionMutationProfile>::new();
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

pub(in crate::runner) fn execution_mutation_profile_from_details(
    metadata: &ToolResultMeta,
) -> Option<ExecutionMutationProfile> {
    metadata
        .details
        .get("execution_mutation_profile")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

pub(in crate::runner) fn terminal_task_id_from_tool_metadata(
    metadata: &ToolResultMeta,
) -> Option<String> {
    metadata
        .details
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

pub(in crate::runner) fn append_terminal_cancel_execution_audit(
    session: &mut Session,
    call: &ToolCall,
    subjects: &[ToolSubject],
    status: ToolExecutionStatus,
    duration_ms: Option<u64>,
    permission_plan: Option<&sigil_kernel::ToolPermissionPlanV2>,
    execution_mutation_profile: Option<&ExecutionMutationProfile>,
    result: Option<&ToolResult>,
) -> anyhow::Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            Vec::new(),
            durable_terminal_tool_result_metadata(&result.metadata),
            error,
            Some(tool_result_model_content_hash(result)),
        )
    } else {
        let mut details = serde_json::json!({
            "call": {
                "summary": format!("task_id={}", terminal_cancel_task_id_from_call(call))
            }
        });
        if let Some(profile) = execution_mutation_profile {
            details["execution_mutation_profile"] = serde_json::to_value(profile)?;
        }
        if let Some(plan) = permission_plan {
            details["permission_plan_hash"] = serde_json::Value::String(plan.plan_hash.clone());
            details["execution_containment"] = serde_json::to_value(&plan.containment)?;
        }
        (
            Vec::new(),
            ToolResultMeta {
                details,
                ..ToolResultMeta::default()
            },
            None,
            None,
        )
    };
    session.append_control(ControlEntry::ToolExecution(Box::new(ToolExecutionEntry {
        call_id: call.id.clone(),
        tool_name: call.name.clone(),
        status,
        duration_ms,
        subjects: subjects.iter().map(ToolSubjectAudit::from).collect(),
        changed_files,
        metadata,
        error,
        model_content_hash,
    })))
}

pub(in crate::runner) fn durable_terminal_tool_result_metadata(
    metadata: &ToolResultMeta,
) -> ToolResultMeta {
    const DURABLE_TERMINAL_DETAIL_KEYS: &[&str] = &[
        "cleanup",
        "command_sha256",
        "created_at_ms",
        "cwd_label",
        "generation",
        "log_ref",
        "output_hash",
        "output_limit_bytes",
        "output_termination_reason",
        "output_total_bytes",
        "output_truncated",
        "readiness",
        "schema_version",
        "shell_label",
        "shell_sha256",
        "status",
        "status_detail",
        "task_id",
        "updated_at_ms",
    ];
    let details = metadata.details.as_object().map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |object| {
            serde_json::Value::Object(
                object
                    .iter()
                    .filter(|(key, value)| {
                        DURABLE_TERMINAL_DETAIL_KEYS.contains(&key.as_str()) && !value.is_null()
                    })
                    .map(|(key, value)| {
                        (
                            key.clone(),
                            sigil_kernel::safe_persistence_json_value(value.clone()),
                        )
                    })
                    .collect(),
            )
        },
    );
    ToolResultMeta {
        details,
        ..ToolResultMeta::default()
    }
}

pub(in crate::runner) fn terminal_cancel_task_id_from_call(call: &ToolCall) -> String {
    serde_json::from_str::<serde_json::Value>(&call.args_json)
        .ok()
        .and_then(|value| {
            value
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(in crate::runner) fn tool_result_model_content_hash(result: &ToolResult) -> String {
    let mut hasher = Sha256::new();
    hasher.update(result.to_model_content().as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(in crate::runner) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(saturating_elapsed(started).as_millis()).unwrap_or(u64::MAX)
}
