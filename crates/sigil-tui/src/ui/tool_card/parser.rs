use super::*;

pub(super) fn parse_tool_summary(text: &str) -> ToolCardRender {
    let fallback = ToolCardRender {
        call_id: None,
        tool_name: "result".to_owned(),
        is_error: false,
        error_kind: None,
        summary: None,
        metadata: ToolCardMetadata::default(),
        preview_kind: ToolPreviewKind::Text,
        preview_language: None,
        preview_lines: text.lines().take(8).map(str::to_owned).collect(),
        hidden_lines: text.lines().count().saturating_sub(8),
        preview_value: None,
        diff: None,
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return fallback;
    };
    let Some(object) = value.as_object() else {
        return fallback;
    };
    let Some(tool_name) = object
        .get("tool_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
    else {
        return fallback;
    };
    let status = object
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("ok")
        .to_uppercase();
    let is_error = status == "ERROR";
    let error_kind = object
        .get("error_kind")
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .get("error")
                .and_then(|error| error.get("kind"))
                .and_then(Value::as_str)
        })
        .map(str::to_owned);
    let metadata = object
        .get("metadata")
        .map(parse_tool_metadata)
        .unwrap_or_default();
    let summary = object
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let preview_kind = object
        .get("preview_kind")
        .and_then(serde_json::Value::as_str)
        .map(ToolPreviewKind::from_value)
        .unwrap_or_default();
    let preview_language = object
        .get("preview_language")
        .and_then(serde_json::Value::as_str)
        .filter(|language| !language.trim().is_empty())
        .map(str::to_owned);
    let preview_value = object.get("preview_value").cloned();
    let (preview_lines, hidden_lines) = object
        .get("preview_lines")
        .and_then(serde_json::Value::as_array)
        .map(|lines| {
            let preview = lines
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let hidden = object
                .get("hidden_lines")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0) as usize;
            (preview, hidden)
        })
        .unwrap_or_default();
    let diff = object.get("diff").and_then(parse_tool_diff);

    ToolCardRender {
        call_id: object
            .get("call_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        tool_name,
        is_error,
        error_kind,
        summary,
        metadata,
        preview_kind,
        preview_language,
        preview_lines,
        hidden_lines,
        preview_value,
        diff,
    }
}

pub(super) fn parse_tool_diff(value: &Value) -> Option<ToolCardDiff> {
    let object = value.as_object()?;
    let files = object
        .get("files")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(parse_tool_diff_file)
        .collect::<Vec<_>>();
    if files.is_empty() {
        return None;
    }
    Some(ToolCardDiff {
        summary: object
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("file diff")
            .to_owned(),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        original_line_count: object
            .get("original_line_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| {
                files
                    .iter()
                    .map(|file| file.original_line_count as u64)
                    .sum()
            }) as usize,
        rendered_line_count: object
            .get("rendered_line_count")
            .and_then(Value::as_u64)
            .unwrap_or_else(|| files.iter().map(|file| file.lines.len() as u64).sum())
            as usize,
        files,
    })
}

pub(super) fn parse_tool_diff_file(value: &Value) -> Option<ToolCardDiffFile> {
    let object = value.as_object()?;
    let lines = object
        .get("lines")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    Some(ToolCardDiffFile {
        path: object
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned(),
        truncated: object
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        original_line_count: object
            .get("original_line_count")
            .and_then(Value::as_u64)
            .unwrap_or(lines.len() as u64) as usize,
        rendered_line_count: object
            .get("rendered_line_count")
            .and_then(Value::as_u64)
            .unwrap_or(lines.len() as u64) as usize,
        lines,
    })
}

pub(super) fn parse_tool_metadata(value: &Value) -> ToolCardMetadata {
    let Some(object) = value.as_object() else {
        return ToolCardMetadata::default();
    };
    let details = object.get("details");
    let call_context = details.and_then(|details| details.get("call"));
    let (subject_mcp_server, subject_mcp_tool, subject_mcp_trust_class) =
        parse_mcp_call_subjects(call_context);
    let terminal_context = details
        .and_then(|details| details.get("terminal_task"))
        .or(details);
    let shell_context = details
        .and_then(|details| details.get("shell_analysis"))
        .or_else(|| details.and_then(|details| details.get("shell")));
    ToolCardMetadata {
        exit_code: object.get("exit_code").and_then(Value::as_i64),
        stdout_bytes: object.get("stdout_bytes").and_then(Value::as_u64),
        stderr_bytes: object.get("stderr_bytes").and_then(Value::as_u64),
        changed_files: object
            .get("changed_files")
            .and_then(Value::as_array)
            .map(|files| {
                files
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        call_summary: object
            .get("details")
            .and_then(|details| details.get("call"))
            .and_then(|call| call.get("summary"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        action: object
            .get("details")
            .and_then(|details| details.get("action"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        mcp_server: subject_mcp_server.or_else(|| {
            details
                .and_then(|details| details.get("mcp"))
                .and_then(|mcp| mcp.get("server"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        mcp_tool: subject_mcp_tool.or_else(|| {
            details
                .and_then(|details| details.get("mcp"))
                .and_then(|mcp| mcp.get("tool"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        mcp_trust_class: subject_mcp_trust_class.or_else(|| {
            details
                .and_then(|details| details.get("mcp"))
                .and_then(|mcp| mcp.get("trust_class"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        code_server: object
            .get("details")
            .and_then(|details| details.get("code_intelligence"))
            .and_then(|details| details.get("server"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        code_capability: object
            .get("details")
            .and_then(|details| details.get("code_intelligence"))
            .and_then(|details| details.get("capability"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        returned_entries: object
            .get("returned_entries")
            .and_then(Value::as_u64)
            .or_else(|| {
                object
                    .get("details")
                    .and_then(|details| details.get("code_intelligence"))
                    .and_then(|details| details.get("returned"))
                    .and_then(Value::as_u64)
            }),
        total_entries: object
            .get("total_entries")
            .and_then(Value::as_u64)
            .or_else(|| {
                object
                    .get("details")
                    .and_then(|details| details.get("code_intelligence"))
                    .and_then(|details| details.get("total"))
                    .and_then(Value::as_u64)
            }),
        execution_backend: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("backend"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        execution_network_policy: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("network"))
            .and_then(|network| network.get("policy"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        execution_timeout_source: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("resources"))
            .and_then(|resources| resources.get("timeout_source"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        execution_cleanup_status: details
            .and_then(|details| details.get("execution"))
            .and_then(|execution| execution.get("resources"))
            .and_then(|resources| resources.get("cleanup"))
            .and_then(|cleanup| cleanup.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        shell_command_family: shell_context
            .and_then(|shell| shell.get("command_family"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        shell_verdict: shell_context
            .and_then(|shell| shell.get("verdict"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_cleanup_status: terminal_context
            .and_then(|details| details.get("cleanup"))
            .and_then(|cleanup| cleanup.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_task_id: terminal_context
            .and_then(|details| details.get("task_id"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_status: terminal_context
            .and_then(|details| details.get("status"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_command: terminal_context
            .and_then(|details| details.get("command"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_log_path: terminal_context
            .and_then(|details| details.get("log_path"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        terminal_exit_code: terminal_context
            .and_then(|details| details.get("status_detail"))
            .and_then(|status| status.get("exit_code"))
            .and_then(Value::as_i64),
        terminal_failed_reason: terminal_context
            .and_then(|details| details.get("status_detail"))
            .and_then(|status| status.get("reason"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

pub(super) fn parse_mcp_call_subjects(
    call_context: Option<&Value>,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut mcp_server = None;
    let mut mcp_tool = None;
    let mut mcp_trust_class = None;
    let Some(subjects) = call_context
        .and_then(|call| call.get("subjects"))
        .and_then(Value::as_array)
    else {
        return (mcp_server, mcp_tool, mcp_trust_class);
    };
    for subject in subjects.iter().filter_map(Value::as_str) {
        let mut parts = subject.splitn(3, ':');
        let _scope = parts.next();
        let Some(kind) = parts.next() else {
            continue;
        };
        let Some(target) = parts.next() else {
            continue;
        };
        match kind {
            "mcp_tool" => {
                if let Some((server, tool)) = parse_mcp_provider_name(target) {
                    mcp_server = mcp_server.or(Some(server));
                    mcp_tool = mcp_tool.or(Some(tool));
                }
            }
            "mcp_trust_class" => {
                if let Some(trust_class) = target.strip_prefix("mcp_trust_class:") {
                    mcp_trust_class = mcp_trust_class.or(Some(trust_class.to_owned()));
                }
            }
            _ => {}
        }
    }
    (mcp_server, mcp_tool, mcp_trust_class)
}
