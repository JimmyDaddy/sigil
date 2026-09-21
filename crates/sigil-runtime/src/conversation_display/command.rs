use super::*;

pub(super) fn command_input(call: &sigil_kernel::ToolCall) -> Option<String> {
    if !matches!(
        call.name.as_str(),
        "exec_command" | "bash" | "terminal_start"
    ) {
        return None;
    }
    let args = serde_json::from_str::<serde_json::Value>(&call.args_json).ok()?;
    args.get("command")
        .and_then(serde_json::Value::as_str)
        .map(|command| project_text(command).text)
}

pub(super) struct CommandResultProjection {
    pub(super) input: Option<String>,
    pub(super) execution_id: Option<String>,
    pub(super) started_at_ms: Option<u64>,
    pub(super) updated_at_ms: Option<u64>,
    pub(super) status: ConversationDisplayStatusV1,
    pub(super) output: Option<String>,
}

pub(super) fn command_result_projection(
    result: &sigil_kernel::ToolResultRecordedV3,
    tool: Option<&ToolProjection>,
    preview: &str,
) -> CommandResultProjection {
    let facts = &result.facts;
    let details = &facts.tool_specific;
    let execution = details.get("terminal_task").unwrap_or(details);
    let phase = execution.get("status").and_then(serde_json::Value::as_str);
    let execution_id = execution_id_from_result(result);
    let status = match (execution_id.is_some(), phase) {
        (true, Some("cancelled")) => ConversationDisplayStatusV1::Cancelled,
        (true, Some("interrupted")) => ConversationDisplayStatusV1::Interrupted,
        (true, Some("failed")) => ConversationDisplayStatusV1::Failed,
        _ if facts.status != "ok" || facts.exit_code.is_some_and(|code| code != 0) => {
            ConversationDisplayStatusV1::Failed
        }
        (true, Some("starting" | "running")) => ConversationDisplayStatusV1::Running,
        _ => ConversationDisplayStatusV1::Completed,
    };
    let input = match tool.filter(|tool| tool.input_json_bytes.is_some()) {
        // The index hydrates this exact source call; it never caches command bodies in metadata.
        Some(tool) => tool.input.clone(),
        None => details
            .pointer("/call/summary")
            .and_then(serde_json::Value::as_str)
            .and_then(|summary| summary.strip_prefix("command="))
            .or_else(|| {
                details
                    .pointer("/shell_analysis/command")
                    .and_then(serde_json::Value::as_str)
            })
            .map(|command| project_text(command).text),
    };
    let output = if result.tool_name.starts_with("exec_") && facts.error.is_some() {
        facts
            .error
            .as_ref()
            .map(|error| project_text(&error.message).text)
    } else if result.tool_name.starts_with("exec_") {
        details
            .get("output_preview")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| project_text(value).text)
    } else {
        Some(preview.to_owned())
    };
    CommandResultProjection {
        input,
        execution_id,
        started_at_ms: details
            .get("started_at_ms")
            .and_then(serde_json::Value::as_u64)
            .or_else(|| tool.and_then(|tool| tool.execution_started_at_ms)),
        updated_at_ms: execution
            .get("updated_at_ms")
            .or_else(|| details.get("updated_at_ms"))
            .and_then(serde_json::Value::as_u64),
        status,
        output,
    }
}

pub(super) fn execution_id_from_result(
    result: &sigil_kernel::ToolResultRecordedV3,
) -> Option<String> {
    let facts = &result.facts;
    let details = &facts.tool_specific;
    let execution = details.get("terminal_task").unwrap_or(details);
    let phase = execution.get("status").and_then(serde_json::Value::as_str);
    let has_execution = result.tool_name.starts_with("exec_")
        && match phase {
            Some("starting" | "running") => facts.status == "ok",
            Some("exited" | "cancelled" | "failed" | "interrupted") => true,
            _ => false,
        };
    has_execution
        .then(|| {
            execution
                .get("execution_id")
                .or_else(|| details.get("execution_id"))
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .map(str::to_owned)
        })
        .flatten()
}

/// Artifact identity and counters are metadata; preview/output bodies stay in source records.
#[derive(Debug, Clone)]
pub(super) struct CommandArtifactProjection {
    artifact_ref: Option<String>,
    artifact_availability: Option<String>,
    observed_bytes: Option<u64>,
    persisted_bytes: Option<u64>,
    has_more: bool,
    preview_truncated: bool,
    truncation_reason: Option<String>,
    capture_completeness: Option<String>,
}

impl CommandArtifactProjection {
    pub(super) fn from_item(item: &ConversationDisplayItemV1) -> Option<Self> {
        let ConversationDisplayContentV1::Tool {
            artifact_ref,
            artifact_availability,
            observed_bytes,
            persisted_bytes,
            has_more,
            preview_truncated,
            truncation_reason,
            capture_completeness,
            ..
        } = &item.content
        else {
            return None;
        };
        Some(Self {
            artifact_ref: artifact_ref.clone(),
            artifact_availability: artifact_availability.clone(),
            observed_bytes: *observed_bytes,
            persisted_bytes: *persisted_bytes,
            has_more: *has_more,
            preview_truncated: *preview_truncated,
            truncation_reason: truncation_reason.clone(),
            capture_completeness: capture_completeness.clone(),
        })
    }

    fn apply(&self, item: &mut ConversationDisplayItemV1) {
        if let ConversationDisplayContentV1::Tool {
            artifact_ref,
            artifact_availability,
            observed_bytes,
            persisted_bytes,
            has_more,
            preview_truncated,
            truncation_reason,
            capture_completeness,
            ..
        } = &mut item.content
        {
            *artifact_ref = self.artifact_ref.clone();
            *artifact_availability = self.artifact_availability.clone();
            *observed_bytes = self.observed_bytes;
            *persisted_bytes = self.persisted_bytes;
            *has_more = self.has_more;
            *preview_truncated |= self.preview_truncated;
            *truncation_reason = self.truncation_reason.clone();
            *capture_completeness = self.capture_completeness.clone();
        }
    }
}

/// Bounded facts only: an execution may settle before its first tool receipt is persisted.
#[derive(Debug, Clone)]
pub(super) struct CommandTerminalState {
    generation: Option<u64>,
    status: ConversationDisplayStatusV1,
    updated_at_ms: Option<u64>,
}

impl CommandTerminalState {
    fn retain(self, id: String, states: &mut HashMap<String, Self>) -> Self {
        let state = states.entry(id).or_insert_with(|| self.clone());
        // A terminal fact never reverts to running. Only a later owner generation may refine it.
        if self.generation.is_some() && self.generation > state.generation {
            *state = self;
        }
        state.clone()
    }
}

pub(super) fn retain_terminal_result(
    result: &sigil_kernel::ToolResultRecordedV3,
    command: &mut CommandResultProjection,
    terminals: &mut HashMap<String, CommandTerminalState>,
) {
    let Some(id) = command.execution_id.as_ref() else {
        return;
    };
    if command.status != ConversationDisplayStatusV1::Running {
        let details = &result.facts.tool_specific;
        CommandTerminalState {
            generation: details
                .get("terminal_task")
                .unwrap_or(details)
                .get("generation")
                .or_else(|| details.get("generation"))
                .and_then(serde_json::Value::as_u64),
            status: command.status,
            updated_at_ms: command.updated_at_ms,
        }
        .retain(id.clone(), terminals);
    }
    if let Some(terminal) = terminals.get(id) {
        command.status = terminal.status;
        command.updated_at_ms = terminal.updated_at_ms;
    }
}

pub(super) fn project_terminal_snapshot(
    record: &SessionStreamRecord,
    scope: &str,
    task: &sigil_kernel::TerminalTaskEntry,
    tools: &mut ToolProjectionState,
) -> Result<Vec<ConversationDisplayItemV1>> {
    if !task.status.is_terminal() {
        return Ok(Vec::new());
    }
    let status = match task.status {
        sigil_kernel::TerminalTaskStatus::Exited { exit_code: Some(0) } => {
            ConversationDisplayStatusV1::Completed
        }
        sigil_kernel::TerminalTaskStatus::Cancelled => ConversationDisplayStatusV1::Cancelled,
        sigil_kernel::TerminalTaskStatus::Interrupted => ConversationDisplayStatusV1::Interrupted,
        _ => ConversationDisplayStatusV1::Failed,
    };
    let id = task.handle.task_id.as_str().to_owned();
    if tools
        .terminals
        .get(&id)
        .and_then(|state| state.generation)
        .is_some_and(|generation| generation >= task.generation)
    {
        // A stale snapshot cannot replace the newer row's output, artifact capabilities or order.
        return Ok(Vec::new());
    }
    let terminal = CommandTerminalState {
        generation: Some(task.generation),
        status,
        updated_at_ms: Some(task.updated_at_ms),
    }
    .retain(id.clone(), &mut tools.terminals);
    let key = ToolProjectionKey::Execution(id);
    let Some(origin) = tools.items.get_mut(&key) else {
        return Ok(Vec::new());
    };
    let mut item = new_item(
        scope,
        record,
        0,
        ConversationDisplayItemKindV1::Tool,
        ConversationDisplaySourceV1::DurableTranscript,
        origin.execution_run_id.clone(),
        terminal.status,
        ConversationDisplayContentV1::Tool {
            call_id: Some(bound_identity(&origin.requested_call_id)),
            tool_name: Some(origin.name.clone()),
            input: origin.input.clone(),
            execution_id: Some(task.handle.task_id.as_str().to_owned()),
            execution_started_at_ms: origin.execution_started_at_ms,
            execution_updated_at_ms: terminal.updated_at_ms,
            output: task
                .output_preview
                .as_deref()
                .map(|value| project_text(value).text),
            truncated: task.output_truncated,
            original_content_bytes: task.output_total_bytes as usize,
            artifact_ref: None,
            artifact_availability: None,
            observed_bytes: None,
            persisted_bytes: None,
            has_more: false,
            preview_truncated: task.output_truncated,
            truncation_reason: None,
            capture_completeness: None,
        },
    );
    if let Some(artifact) = &origin.execution_artifact {
        artifact.apply(&mut item);
    }
    item.reconciles = origin
        .latest_execution_display_id
        .clone()
        .map(|id| vec![id]);
    origin.latest_execution_display_id = Some(item.display_id.clone());
    Ok(vec![item])
}
