use super::*;

pub(in crate::runner) fn refresh_terminal_task_statuses(
    runtime: &tokio::runtime::Runtime,
    registry: &ToolRegistry,
    options: &AgentRunOptions,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
) -> std::result::Result<Vec<(TerminalTaskEntry, Vec<SessionLogEntry>)>, String> {
    let Some(session) = current_session.as_mut() else {
        return Ok(Vec::new());
    };
    let active_task_ids = session.terminal_task_projection().active_task_ids;
    if active_task_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mutation_recorder = MutationEventRecorder::new(
        JsonlSessionStore::new(current_session_log_path)
            .map_err(|error| format!("failed to open mutation recorder: {error:#}"))?,
    );
    let tool_context = ToolContext::new(options.workspace_root.clone(), options.tool_timeout_secs)
        .with_mutation_recorder(mutation_recorder.clone());
    let mut updates = Vec::new();
    for task_id in active_task_ids {
        let call = ToolCall {
            id: format!("tui-terminal-refresh-{}", task_id.as_str()),
            name: "terminal_read".to_owned(),
            args_json: serde_json::json!({
                "task_id": task_id.as_str(),
                "limit_bytes": 1
            })
            .to_string(),
        };
        let result = match runtime.block_on(registry.execute(tool_context.clone(), call)) {
            Ok(result) if !result.is_error() => result,
            Ok(_) | Err(_) => continue,
        };
        let Some(entry) = terminal_read_latest_entry(&result)? else {
            continue;
        };
        if !entry.status.is_terminal() {
            continue;
        }

        session
            .append_control(ControlEntry::TerminalTask(entry.clone()))
            .map_err(|error| format!("failed to append terminal task state: {error:#}"))?;
        if let Some(profile) =
            terminal_start_execution_profile_for_task(session.entries(), &entry.handle.task_id)
        {
            mutation_recorder
                .reconcile_execution_mutation_profile(&options.workspace_root, &profile)
                .map_err(|error| {
                    format!("failed to reconcile terminal task workspace mutation: {error:#}")
                })?;
        }
        updates.push((entry, session.entries().to_vec()));
    }
    Ok(updates)
}

pub(in crate::runner) fn cancel_terminal_task(
    runtime: &tokio::runtime::Runtime,
    registry: ToolRegistry,
    root_config: &RootConfig,
    options: &AgentRunOptions,
    current_session_log_path: &Path,
    current_session: &mut Option<Session>,
    task_id: String,
) -> std::result::Result<(TerminalTaskEntry, Vec<SessionLogEntry>), String> {
    let mut session = load_session(
        &root_config.agent.provider,
        &root_config.agent.model,
        current_session_log_path,
    )
    .map_err(|error| format!("failed to load session before terminal cancel: {error:#}"))?;
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
    let subjects = registry
        .permission_subjects(&tool_context, &call)
        .map_err(|error| format!("invalid terminal cancel arguments: {error:#}"))?;
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
        cancel_mutation_profile.as_ref(),
        None,
    )
    .map_err(|error| format!("failed to append terminal cancel audit: {error:#}"))?;

    let execution_started = Instant::now();
    let result = match runtime
        .block_on(registry.execute_after_started_audit(tool_context.clone(), call.clone()))
    {
        Ok(result) => result,
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

pub(in crate::runner) fn terminal_read_latest_entry(
    result: &ToolResult,
) -> std::result::Result<Option<TerminalTaskEntry>, String> {
    let Some(details) = result.metadata.details.get("terminal_task") else {
        return Ok(None);
    };
    TerminalTaskEntry::from_tool_result_details(details)
        .map_err(|error| format!("invalid terminal read status result: {error:#}"))
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
    execution_mutation_profile: Option<&ExecutionMutationProfile>,
    result: Option<&ToolResult>,
) -> anyhow::Result<()> {
    let (changed_files, metadata, error, model_content_hash) = if let Some(result) = result {
        let error = match &result.status {
            ToolResultStatus::Ok => None,
            ToolResultStatus::Error(error) => Some(error.clone()),
        };
        (
            result.metadata.changed_files.clone(),
            result.metadata.clone(),
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
