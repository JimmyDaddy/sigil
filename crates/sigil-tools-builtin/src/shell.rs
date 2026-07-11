use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    ExecutionBackend, ExecutionCleanupStatus, ExecutionOutputReceipt, ExecutionReceipt,
    ExecutionRequest, ExecutionStreamCapture, ExecutionTerminationCause, Tool, ToolAccess,
    ToolCategory, ToolContext, ToolErrorKind, ToolOperation, ToolPreviewCapability, ToolResult,
    ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectScope,
};
use tree_sitter::{Node, Parser};

use crate::{
    constants::{DEFAULT_TEXT_LIMIT_BYTES, HARD_TEXT_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV},
    path::{
        ResolvedToolPath, absolute_path_from, canonical_workspace_root, resolve_tool_path_from_base,
    },
    support::{
        TextLimitResult, ceil_char_boundary, floor_char_boundary, limit_text_head_tail,
        required_string,
    },
};

pub(crate) struct BashTool {
    pub(crate) scratch_root: PathBuf,
    pub(crate) scratch_label: String,
    pub(crate) backend: Arc<dyn ExecutionBackend>,
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_owned(),
            description: format!(
                "Run a shell command from the workspace root. Use ${SIGIL_SCRATCH_DIR_ENV} for temporary shell files (shown as {}); OS temp directories are outside the workspace and require permission.external_directory.",
                self.scratch_label
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let command = required_string(args, "command")?;
        Ok(analyze_shell_command(&ctx.workspace_root, command)?.subjects)
    }

    fn permission_access(&self, ctx: &ToolContext, args: &Value) -> Result<ToolAccess> {
        let command = required_string(args, "command")?;
        Ok(analyze_shell_command(&ctx.workspace_root, command)?.access)
    }

    fn permission_operation(&self, ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let command = required_string(args, "command")?;
        Ok(analyze_shell_command(&ctx.workspace_root, command)?.operation)
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let _process_effect = ctx.begin_forward_effect(sigil_kernel::RunEffectKind::Process)?;
        let command = required_string(&args, "command")?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(ctx.timeout_secs);
        let scratch_root = absolute_path_from(&ctx.workspace_root, &self.scratch_root);
        tokio::fs::create_dir_all(&scratch_root)
            .await
            .with_context(|| format!("failed to create {}", self.scratch_label))?;
        let analysis = analyze_shell_command(&ctx.workspace_root, command)?;
        let request =
            bash_execution_request(command, &ctx.workspace_root, &scratch_root, timeout_secs);
        let receipt = self
            .backend
            .execute_with_cancellation(request, ctx.cancellation_handle())
            .await?;
        if matches!(
            receipt.effective_output().termination,
            ExecutionTerminationCause::Cancelled
        ) && receipt.resources.cleanup.status != ExecutionCleanupStatus::Completed
            && let Some(cancellation) = ctx.cancellation_handle()
        {
            cancellation.mark_cleanup_incomplete();
        }
        bash_tool_result_from_execution_receipt_with_analysis(
            call_id,
            self.spec().name,
            receipt,
            &analysis,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellCommandAnalysis {
    pub(crate) command: String,
    pub(crate) normalized_command: String,
    pub(crate) command_family: CommandFamily,
    pub(crate) classification_source: ShellClassificationSource,
    pub(crate) access: ToolAccess,
    pub(crate) operation: ToolOperation,
    pub(crate) subjects: Vec<ToolSubject>,
    pub(crate) grant_scope: Option<CommandGrantScope>,
    pub(crate) explanation: ShellApprovalReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandFamily {
    CargoCheck,
    CargoFmtCheck,
    CargoTest,
    CheckTouched { tier: Option<String> },
    GitReadOnly,
    Search,
    ListRead,
    Unknown,
}

impl CommandFamily {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::CargoCheck => "cargo_check",
            Self::CargoFmtCheck => "cargo_fmt_check",
            Self::CargoTest => "cargo_test",
            Self::CheckTouched { .. } => "check_touched",
            Self::GitReadOnly => "git_read_only",
            Self::Search => "search",
            Self::ListRead => "list_read",
            Self::Unknown => "unknown",
        }
    }

    fn stable_subject(&self) -> String {
        match self {
            Self::CheckTouched { tier } => tier
                .as_deref()
                .map(|tier| format!("family:check_touched:{tier}"))
                .unwrap_or_else(|| "family:check_touched".to_owned()),
            _ => format!("family:{}", self.as_str()),
        }
    }

    pub(crate) fn is_workspace_check(&self) -> bool {
        matches!(
            self,
            Self::CargoCheck | Self::CargoFmtCheck | Self::CargoTest | Self::CheckTouched { .. }
        )
    }

    fn is_workspace_read_only(&self) -> bool {
        matches!(self, Self::GitReadOnly | Self::Search | Self::ListRead)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellClassificationSource {
    BuiltinFamily,
    KnownReadonlyFastPath,
    AstKnownReadonly,
    DestructivePattern,
    Unknown,
}

impl ShellClassificationSource {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::BuiltinFamily => "builtin_family",
            Self::KnownReadonlyFastPath => "known_readonly_fast_path",
            Self::AstKnownReadonly => "ast_known_readonly",
            Self::DestructivePattern => "destructive_pattern",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommandGrantScope {
    ExactCommand,
    WorkspaceCheckFamily,
    WorkspaceReadOnlyShell,
    WorkspaceScript {
        path: String,
        args_family: Option<String>,
    },
}

impl CommandGrantScope {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::ExactCommand => "exact_command",
            Self::WorkspaceCheckFamily => "workspace_check_family",
            Self::WorkspaceReadOnlyShell => "workspace_read_only_shell",
            Self::WorkspaceScript { .. } => "workspace_script",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ShellApprovalReason {
    WorkspaceCheck,
    WorkspaceReadOnly,
    UnknownCommand,
    DestructiveCommand,
}

impl ShellApprovalReason {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::WorkspaceCheck => "workspace_check",
            Self::WorkspaceReadOnly => "workspace_read_only",
            Self::UnknownCommand => "unknown_command",
            Self::DestructiveCommand => "destructive_command",
        }
    }
}

pub(crate) fn analyze_shell_command(
    workspace_root: &Path,
    command: &str,
) -> Result<ShellCommandAnalysis> {
    let family = classify_shell_command_family(workspace_root, command)?;
    let normalized_command = normalize_shell_command_for_permission(command);
    let destructive = shell_command_is_destructive(command);
    let ast_known_readonly = !destructive
        && family == CommandFamily::Unknown
        && bash_command_is_ast_known_readonly(command);
    let mut subjects = Vec::new();
    let access;
    let operation;
    let grant_scope;
    let explanation;
    let classification_source;

    if destructive {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteDestructiveCommand;
        grant_scope = None;
        explanation = ShellApprovalReason::DestructiveCommand;
        classification_source = ShellClassificationSource::DestructivePattern;
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            command_permission_subject(command),
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    } else if family.is_workspace_check() {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteWorkspaceCheckCommand;
        grant_scope = workspace_check_grant_scope(&family);
        explanation = ShellApprovalReason::WorkspaceCheck;
        classification_source = ShellClassificationSource::BuiltinFamily;
        let stable_subject = family.stable_subject();
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            stable_subject,
        ));
        subjects.extend(external_shell_path_subjects(workspace_root, command)?);
    } else if family.is_workspace_read_only()
        || ast_known_readonly
        || bash_command_is_safe_readonly(command)
    {
        access = ToolAccess::Read;
        operation = ToolOperation::ExecuteReadOnlyCommand;
        grant_scope = if family == CommandFamily::Unknown {
            Some(CommandGrantScope::ExactCommand)
        } else {
            Some(CommandGrantScope::WorkspaceReadOnlyShell)
        };
        explanation = ShellApprovalReason::WorkspaceReadOnly;
        classification_source = if ast_known_readonly {
            ShellClassificationSource::AstKnownReadonly
        } else if family == CommandFamily::Unknown {
            ShellClassificationSource::KnownReadonlyFastPath
        } else {
            ShellClassificationSource::BuiltinFamily
        };
        let stable_subject = if family == CommandFamily::Unknown {
            command_permission_subject(command)
        } else {
            family.stable_subject()
        };
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            stable_subject,
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    } else {
        access = ToolAccess::Execute;
        operation = ToolOperation::ExecuteUnknownCommand;
        grant_scope = None;
        explanation = ShellApprovalReason::UnknownCommand;
        classification_source = ShellClassificationSource::Unknown;
        subjects.push(ToolSubject::command(
            normalized_command.clone(),
            command_permission_subject(command),
        ));
        subjects.extend(bash_path_subjects(workspace_root, command)?);
    }

    Ok(ShellCommandAnalysis {
        command: command.to_owned(),
        normalized_command,
        command_family: family,
        classification_source,
        access,
        operation,
        subjects,
        grant_scope,
        explanation,
    })
}

fn workspace_check_grant_scope(family: &CommandFamily) -> Option<CommandGrantScope> {
    match family {
        CommandFamily::CheckTouched { tier } => Some(CommandGrantScope::WorkspaceScript {
            path: "scripts/check-touched.sh".to_owned(),
            args_family: tier.clone(),
        }),
        _ => Some(CommandGrantScope::WorkspaceCheckFamily),
    }
}

pub(crate) fn bash_execution_request(
    command: &str,
    workspace_root: &Path,
    scratch_root: &Path,
    timeout_secs: u64,
) -> ExecutionRequest {
    ExecutionRequest {
        program: "sh".to_owned(),
        args: vec!["-c".to_owned(), command.to_owned()],
        cwd: workspace_root.to_path_buf(),
        env: BTreeMap::from([(
            SIGIL_SCRATCH_DIR_ENV.to_owned(),
            scratch_root.to_string_lossy().into_owned(),
        )]),
        environment_policy: sigil_kernel::ProcessEnvironmentPolicy::InheritParent,
        timeout_ms: None,
        timeout_secs,
        cpu_time_ms: None,
        memory_limit_bytes: None,
        process_count_limit: None,
    }
}

#[cfg(test)]
pub(crate) fn bash_tool_result_from_execution_receipt(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
) -> Result<ToolResult> {
    bash_tool_result_from_execution_receipt_inner(call_id, tool_name, receipt, None)
}

pub(crate) fn bash_tool_result_from_execution_receipt_with_analysis(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
    analysis: &ShellCommandAnalysis,
) -> Result<ToolResult> {
    bash_tool_result_from_execution_receipt_inner(call_id, tool_name, receipt, Some(analysis))
}

fn bash_tool_result_from_execution_receipt_inner(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
    analysis: Option<&ShellCommandAnalysis>,
) -> Result<ToolResult> {
    let output = receipt.effective_output();
    let limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
    let limited_stdout = captured_stream_text(&receipt.stdout, &output.stdout, limit_bytes);
    let limited_stderr = captured_stream_text(&receipt.stderr, &output.stderr, limit_bytes);
    let mut content = String::new();
    if !limited_stdout.content.is_empty() {
        content.push_str(&limited_stdout.content);
    }
    if !limited_stderr.content.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&limited_stderr.content);
    }
    let output_truncated = output.stdout.truncated
        || output.stderr.truncated
        || limited_stdout.truncated
        || limited_stderr.truncated;
    let tail_available = output.stdout.retained_tail_bytes > 0
        || output.stderr.retained_tail_bytes > 0
        || output_truncated;
    let metadata = ToolResultMeta {
        exit_code: receipt.exit_code,
        stdout_bytes: Some(output.stdout.total_bytes),
        stderr_bytes: Some(output.stderr.total_bytes),
        truncated: output_truncated,
        omitted_bytes: Some(
            limited_stdout
                .omitted_bytes
                .saturating_add(limited_stderr.omitted_bytes),
        ),
        limit_bytes: Some(limit_bytes as u64),
        returned_bytes: Some(
            limited_stdout
                .returned_bytes
                .saturating_add(limited_stderr.returned_bytes),
        ),
        total_bytes: Some(output.combined_total_bytes),
        returned_lines: Some(limited_stdout.returned_lines + limited_stderr.returned_lines),
        total_lines: Some(
            output
                .stdout
                .total_lines
                .saturating_add(output.stderr.total_lines),
        ),
        details: execution_receipt_details(&receipt, analysis, output_truncated, tail_available),
        ..ToolResultMeta::default()
    };
    if let Some((kind, message)) = execution_termination_error(&output.termination) {
        let details = metadata.details.clone();
        let mut result =
            ToolResult::error(call_id, tool_name, kind, message).with_error_details(false, details);
        if !content.is_empty() {
            result.content = content;
        }
        result.metadata = metadata;
        return Ok(result);
    }
    if receipt.exit_code == Some(0) {
        Ok(ToolResult::ok(call_id, tool_name, content, metadata))
    } else {
        let mut result = ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::ExitStatus,
            if content.is_empty() {
                "bash command exited with non-zero status".to_owned()
            } else {
                content.clone()
            },
        );
        result.content = content;
        result.metadata = metadata;
        Ok(result)
    }
}

fn captured_stream_text(
    bytes: &[u8],
    capture: &ExecutionStreamCapture,
    fallback_limit_bytes: usize,
) -> TextLimitResult {
    if !capture.truncated {
        let text = String::from_utf8_lossy(bytes);
        let mut limited = limit_text_head_tail(&text, fallback_limit_bytes);
        if limited.content.len() > fallback_limit_bytes {
            limited.content = bounded_text_projection(
                &text,
                fallback_limit_bytes,
                capture
                    .total_bytes
                    .saturating_sub(capture.returned_bytes.min(fallback_limit_bytes as u64)),
            );
        }
        limited.total_bytes = capture.total_bytes;
        limited.total_lines = capture.total_lines;
        limited.returned_bytes = limited
            .returned_bytes
            .min(capture.returned_bytes)
            .min(fallback_limit_bytes as u64);
        limited.omitted_bytes = capture.total_bytes.saturating_sub(limited.returned_bytes);
        return limited;
    }

    let head_len = usize::try_from(capture.retained_head_bytes)
        .unwrap_or(bytes.len())
        .min(bytes.len());
    let tail_len = usize::try_from(capture.retained_tail_bytes)
        .unwrap_or(bytes.len().saturating_sub(head_len))
        .min(bytes.len().saturating_sub(head_len));
    let head = String::from_utf8_lossy(&bytes[..head_len]);
    let tail = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(tail_len)..]);
    let retained = format!("{head}{tail}");
    let content = bounded_text_projection(&retained, fallback_limit_bytes, capture.omitted_bytes);
    let returned_bytes = capture
        .returned_bytes
        .min(fallback_limit_bytes as u64)
        .min(capture.total_bytes);
    TextLimitResult {
        returned_bytes,
        returned_lines: content.lines().count() as u64,
        total_bytes: capture.total_bytes,
        total_lines: capture.total_lines,
        truncated: true,
        omitted_bytes: capture.total_bytes.saturating_sub(returned_bytes),
        content,
    }
}

fn bounded_text_projection(input: &str, max_bytes: usize, omitted_bytes: u64) -> String {
    let notice = format!("[sigil: output truncated, omitted {omitted_bytes} bytes]");
    if max_bytes <= notice.len() {
        let end = floor_char_boundary(&notice, max_bytes);
        return notice[..end].to_owned();
    }
    let separators = 2usize;
    let raw_budget = max_bytes.saturating_sub(notice.len() + separators);
    let head_budget = raw_budget / 2;
    let tail_budget = raw_budget.saturating_sub(head_budget);
    let head_end = floor_char_boundary(input, head_budget.min(input.len()));
    let tail_start =
        ceil_char_boundary(input, input.len().saturating_sub(tail_budget)).max(head_end);
    format!("{}\n{notice}\n{}", &input[..head_end], &input[tail_start..])
}

fn execution_termination_error(
    termination: &ExecutionTerminationCause,
) -> Option<(ToolErrorKind, &'static str)> {
    match termination {
        ExecutionTerminationCause::Exited => None,
        ExecutionTerminationCause::TimedOut => {
            Some((ToolErrorKind::Timeout, "bash command timed out"))
        }
        ExecutionTerminationCause::Cancelled => Some((
            ToolErrorKind::Interrupted,
            "bash command interrupted by run cancellation",
        )),
        ExecutionTerminationCause::OutputLimit { .. } => Some((
            ToolErrorKind::ResourceLimit,
            "bash command exceeded the output limit",
        )),
        ExecutionTerminationCause::ReaderFailed { .. } => {
            Some((ToolErrorKind::Io, "bash command output reader failed"))
        }
    }
}

pub(crate) fn execution_receipt_details(
    receipt: &ExecutionReceipt,
    analysis: Option<&ShellCommandAnalysis>,
    output_truncated: bool,
    tail_available: bool,
) -> Value {
    let output = receipt.effective_output();
    let mut details = json!({
        "execution": {
            "backend": receipt.backend,
            "capabilities": receipt.capabilities,
            "network": receipt.network,
            "resources": receipt.resources,
        }
    });
    if !matches!(output.termination, ExecutionTerminationCause::Exited)
        || output.stdout.truncated
        || output.stderr.truncated
    {
        details["execution"]["output"] = execution_output_details(&output);
    }
    if let Some(analysis) = analysis {
        details["shell"] = json!({
            "command": analysis.command.as_str(),
            "normalized_command": analysis.normalized_command.as_str(),
            "command_family": analysis.command_family.as_str(),
            "classification_source": analysis.classification_source.as_str(),
            "grant_scope": analysis.grant_scope.as_ref().map(CommandGrantScope::as_str),
            "grant_scope_detail": shell_grant_scope_detail(analysis.grant_scope.as_ref()),
            "approval_reason": analysis.explanation.as_str(),
            "exit_code": receipt.exit_code,
            "verdict": shell_verdict(receipt),
            "output_truncated": output_truncated,
            "tail_available": tail_available,
            "rerun_not_needed": shell_rerun_not_needed(analysis, receipt),
        });
    }
    details
}

fn execution_output_details(output: &ExecutionOutputReceipt) -> Value {
    let mut details = json!({
        "termination": output.termination.as_str(),
        "stdout": &output.stdout,
        "stderr": &output.stderr,
        "combined_total_bytes": output.combined_total_bytes,
        "combined_hard_limit_bytes": output.combined_hard_limit_bytes,
    });
    match &output.termination {
        ExecutionTerminationCause::OutputLimit {
            stream,
            limit_bytes,
            observed_bytes,
        } => {
            details["code"] = json!("output_limit_exceeded");
            details["stream"] = json!(stream.as_str());
            details["limit_bytes"] = json!(limit_bytes);
            details["observed_bytes"] = json!(observed_bytes);
        }
        ExecutionTerminationCause::ReaderFailed { stream, reason } => {
            details["code"] = json!("output_reader_failed");
            details["stream"] = json!(stream.as_str());
            details["reason"] = json!(reason);
        }
        ExecutionTerminationCause::TimedOut => {
            details["code"] = json!("execution_timeout");
        }
        ExecutionTerminationCause::Cancelled => {
            details["code"] = json!("execution_cancelled");
        }
        ExecutionTerminationCause::Exited => {}
    }
    details
}

pub(crate) fn shell_grant_scope_detail(scope: Option<&CommandGrantScope>) -> Value {
    match scope {
        Some(CommandGrantScope::WorkspaceScript { path, args_family }) => json!({
            "path": path,
            "args_family": args_family,
        }),
        _ => Value::Null,
    }
}

fn shell_verdict(receipt: &ExecutionReceipt) -> &'static str {
    match receipt.effective_output().termination {
        ExecutionTerminationCause::TimedOut => "timed_out",
        ExecutionTerminationCause::Cancelled => "interrupted",
        ExecutionTerminationCause::OutputLimit { .. } => "resource_limited",
        ExecutionTerminationCause::ReaderFailed { .. } => "output_reader_failed",
        ExecutionTerminationCause::Exited => match receipt.exit_code {
            Some(0) => "passed",
            Some(_) => "failed",
            None => "unknown",
        },
    }
}

fn shell_rerun_not_needed(analysis: &ShellCommandAnalysis, receipt: &ExecutionReceipt) -> bool {
    analysis.command_family.is_workspace_check()
        && receipt.exit_code == Some(0)
        && matches!(
            receipt.effective_output().termination,
            ExecutionTerminationCause::Exited
        )
}

pub(crate) fn command_permission_subject(command: &str) -> String {
    const MAX_CHARS: usize = 120;
    let normalized = normalize_shell_command_for_permission(command);
    let char_count = normalized.chars().count();
    if char_count <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

pub(crate) fn normalize_shell_command_for_permission(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn classify_shell_command_family(workspace_root: &Path, command: &str) -> Result<CommandFamily> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let tokens = strip_workspace_cd_prefix(&workspace_root, tokenize_shell_subject_words(command))?;
    if tokens.is_empty() {
        return Ok(CommandFamily::Unknown);
    }
    let command_segments = split_shell_command_segments(&tokens);
    if command_segments.is_empty() {
        return Ok(CommandFamily::Unknown);
    }
    if command_segments.len() == 2
        && command_family_for_pipeline(command_segments[0]) == CommandFamily::ListRead
        && shell_segment_is_exit_echo(command_segments[1])
    {
        return Ok(CommandFamily::ListRead);
    }
    if command_segments.len() != 1 {
        return Ok(CommandFamily::Unknown);
    }
    Ok(command_family_for_pipeline(command_segments[0]))
}

fn strip_workspace_cd_prefix(workspace_root: &Path, tokens: Vec<String>) -> Result<Vec<String>> {
    let Some(separator_index) = tokens
        .iter()
        .position(|token| matches!(token.as_str(), "&&" | ";"))
    else {
        return Ok(tokens);
    };
    let prefix = &tokens[..separator_index];
    if !matches!(prefix.first().map(String::as_str), Some("cd")) {
        return Ok(tokens);
    }
    let Some(target) = prefix.get(1).filter(|target| !target.starts_with('-')) else {
        return Ok(tokens);
    };
    let resolved = resolve_tool_path_from_base(workspace_root, workspace_root, target)?;
    if resolved.scope != ToolSubjectScope::Workspace {
        return Ok(tokens);
    }
    Ok(tokens[separator_index + 1..].to_vec())
}

fn split_shell_command_segments(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if start < index {
                segments.push(&tokens[start..index]);
            }
            start = index.saturating_add(1);
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn command_family_for_pipeline(tokens: &[String]) -> CommandFamily {
    let pipeline = split_shell_pipeline(tokens);
    let Some(primary) = pipeline.first().copied() else {
        return CommandFamily::Unknown;
    };
    if pipeline.len() > 2 {
        return CommandFamily::Unknown;
    }
    if let Some(filter) = pipeline.get(1)
        && !shell_segment_is_read_filter(filter)
    {
        return CommandFamily::Unknown;
    }
    command_family_for_simple_segment(primary)
}

fn split_shell_pipeline(tokens: &[String]) -> Vec<&[String]> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        if token == "|" {
            if start < index {
                segments.push(&tokens[start..index]);
            }
            start = index.saturating_add(1);
        }
    }
    if start < tokens.len() {
        segments.push(&tokens[start..]);
    }
    segments
}

fn command_family_for_simple_segment(tokens: &[String]) -> CommandFamily {
    let words = tokens
        .iter()
        .filter(|token| !is_fd_duplication_token(token))
        .cloned()
        .collect::<Vec<_>>();
    let Some((command, args)) = shell_segment_command_and_args(&words) else {
        return CommandFamily::Unknown;
    };
    match command {
        "cargo" => cargo_command_family(args),
        "git" if git_segment_is_safe_readonly(&words) => CommandFamily::GitReadOnly,
        "grep" | "rg" if search_segment_is_read_only(command, args) => CommandFamily::Search,
        "find" if find_segment_is_safe_readonly(&words) => CommandFamily::Search,
        "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "du" | "file" | "readlink"
        | "realpath" | "basename" | "dirname" | "diff" | "cmp" | "pwd" => CommandFamily::ListRead,
        command if command.ends_with("check-touched.sh") => CommandFamily::CheckTouched {
            tier: check_touched_tier(args),
        },
        _ => CommandFamily::Unknown,
    }
}

fn cargo_command_family(args: &[String]) -> CommandFamily {
    match args.first().map(String::as_str) {
        Some("check") => CommandFamily::CargoCheck,
        Some("test") => CommandFamily::CargoTest,
        Some("fmt") if args.iter().skip(1).any(|arg| arg == "--check") => {
            CommandFamily::CargoFmtCheck
        }
        _ => CommandFamily::Unknown,
    }
}

fn check_touched_tier(args: &[String]) -> Option<String> {
    args.iter().enumerate().find_map(|(index, arg)| {
        arg.strip_prefix("--tier=").map(str::to_owned).or_else(|| {
            (arg == "--tier")
                .then(|| args.get(index + 1).cloned())
                .flatten()
        })
    })
}

fn shell_segment_is_read_filter(tokens: &[String]) -> bool {
    let words = tokens
        .iter()
        .filter(|token| !is_fd_duplication_token(token))
        .cloned()
        .collect::<Vec<_>>();
    let Some((command, _args)) = shell_segment_command_and_args(&words) else {
        return false;
    };
    if !shell_segment_redirections_are_readonly(&words) {
        return false;
    }
    match command {
        "head" | "tail" | "wc" | "cat" => true,
        "sort" => words.iter().skip(1).all(|word| {
            word.starts_with('-')
                && !matches!(word.as_str(), "-o" | "--output")
                && !word.starts_with("-o")
                && !word.starts_with("--output=")
        }),
        "uniq" => words.iter().skip(1).all(|word| word.starts_with('-')),
        _ => false,
    }
}

fn search_segment_is_read_only(_command: &str, _args: &[String]) -> bool {
    true
}

fn shell_segment_is_exit_echo(tokens: &[String]) -> bool {
    matches!(tokens.first().map(String::as_str), Some("echo"))
        && tokens
            .iter()
            .skip(1)
            .all(|token| token == "EXIT=$?" || !token.contains('>'))
}

fn is_fd_duplication_token(token: &str) -> bool {
    matches!(token, "2>&1" | "1>&2" | ">&2" | ">&1")
}

fn external_shell_path_subjects(workspace_root: &Path, command: &str) -> Result<Vec<ToolSubject>> {
    Ok(bash_path_subjects(workspace_root, command)?
        .into_iter()
        .filter(|subject| subject.scope == ToolSubjectScope::External)
        .collect())
}

#[cfg(test)]
pub(crate) fn shell_command_permission_operation(command: &str) -> ToolOperation {
    if shell_command_is_destructive(command) {
        ToolOperation::ExecuteDestructiveCommand
    } else if bash_command_is_safe_readonly(command) {
        ToolOperation::ExecuteReadOnlyCommand
    } else {
        ToolOperation::ExecuteUnknownCommand
    }
}

pub(crate) fn terminal_input_permission_operation(input: &str) -> ToolOperation {
    if shell_command_is_destructive(input) {
        ToolOperation::ExecuteDestructiveCommand
    } else {
        ToolOperation::SendTerminalInput
    }
}

pub(crate) fn terminal_input_permission_operation_from_analysis(
    workspace_root: &Path,
    input: &str,
) -> Result<ToolOperation> {
    let analysis = analyze_shell_command(workspace_root, input)?;
    Ok(match analysis.operation {
        ToolOperation::ExecuteDestructiveCommand => ToolOperation::ExecuteDestructiveCommand,
        ToolOperation::ExecuteReadOnlyCommand => ToolOperation::ExecuteReadOnlyCommand,
        ToolOperation::ExecuteWorkspaceCheckCommand => ToolOperation::ExecuteWorkspaceCheckCommand,
        _ => terminal_input_permission_operation(input),
    })
}

pub(crate) fn shell_command_is_destructive(command: &str) -> bool {
    let tokens = tokenize_shell_subject_words(command);
    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if shell_segment_is_destructive(&segment) {
                return true;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    shell_segment_is_destructive(&segment)
}

pub(crate) fn shell_segment_is_destructive(words: &[String]) -> bool {
    let Some((command, args)) = shell_segment_command_and_args(words) else {
        return false;
    };

    if matches!(command, "sudo" | "doas" | "env" | "command") && !args.is_empty() {
        return shell_segment_is_destructive(args);
    }

    if shell_segment_has_overwrite_redirection(words) {
        return true;
    }

    match command {
        "rm" => true,
        "rmdir" => true,
        "truncate" => true,
        "dd" => args.iter().any(|word| word.starts_with("of=")),
        "find" => find_segment_is_destructive(args),
        "git" => git_segment_is_destructive(args),
        "sh" | "bash" | "zsh" | "fish" => shell_invocation_is_destructive(args),
        _ => false,
    }
}

pub(crate) fn shell_segment_command_and_args(words: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0usize;
    while let Some(word) = words.get(index) {
        if is_shell_assignment(word) {
            index += 1;
            continue;
        }
        return Some((shell_command_basename(word), &words[index + 1..]));
    }
    None
}

pub(crate) fn is_shell_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
}

pub(crate) fn shell_command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

pub(crate) fn shell_segment_has_overwrite_redirection(words: &[String]) -> bool {
    let mut index = 0usize;
    while index < words.len() {
        let word = &words[index];
        if overwrite_redirection_target(word) {
            return true;
        }
        if is_overwrite_redirection_operator(word) {
            if overwrite_redirection_operator_target_is_destructive(
                words.get(index + 1).map(String::as_str),
            ) {
                return true;
            }
            index += 1;
        }
        index += 1;
    }
    false
}

pub(crate) fn is_overwrite_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | "1>" | "1>>" | "2>" | "2>>" | "&>" | "&>>"
    )
}

pub(crate) fn overwrite_redirection_target(word: &str) -> bool {
    ["1>>", "1>", "2>>", "2>", "&>>", "&>", ">>", ">"]
        .iter()
        .any(|prefix| {
            word.strip_prefix(prefix).is_some_and(|target| {
                !target.is_empty()
                    && !target.starts_with('&')
                    && !shell_requested_path_is_safe_device(target)
            })
        })
}

fn overwrite_redirection_operator_target_is_destructive(target: Option<&str>) -> bool {
    target.is_none_or(|target| {
        !target.starts_with('&') && !shell_requested_path_is_safe_device(target)
    })
}

pub(crate) fn find_segment_is_destructive(words: &[String]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        word == "-delete"
            || matches!(word.as_str(), "-exec" | "-execdir")
                && words
                    .get(index + 1)
                    .map(|command| shell_command_basename(command) == "rm")
                    .unwrap_or(false)
    })
}

pub(crate) fn git_segment_is_destructive(words: &[String]) -> bool {
    let Some(subcommand) = words.first().map(String::as_str) else {
        return false;
    };
    match subcommand {
        "clean" => true,
        "reset" => words.iter().skip(1).any(|word| word == "--hard"),
        "checkout" | "restore" => words
            .iter()
            .skip(1)
            .any(|word| word == "-f" || word == "--force"),
        _ => false,
    }
}

pub(crate) fn shell_invocation_is_destructive(words: &[String]) -> bool {
    words.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-c" | "-lc") && shell_command_is_destructive(&pair[1])
    })
}

pub(crate) fn bash_command_is_ast_known_readonly(command: &str) -> bool {
    let trimmed = command.trim();
    !trimmed.is_empty()
        && bash_ast_has_supported_readonly_structure(trimmed)
        && bash_command_is_safe_readonly(trimmed)
}

fn bash_ast_has_supported_readonly_structure(command: &str) -> bool {
    let mut parser = Parser::new();
    let language = tree_sitter_bash::LANGUAGE;
    if parser.set_language(&language.into()).is_err() {
        return false;
    }
    let Some(tree) = parser.parse(command, None) else {
        return false;
    };
    let root = tree.root_node();
    if root.has_error() {
        return false;
    }
    let mut saw_readonly_structure = false;
    bash_ast_node_is_supported_readonly_candidate(root, &mut saw_readonly_structure)
        && saw_readonly_structure
}

fn bash_ast_node_is_supported_readonly_candidate(
    node: Node<'_>,
    saw_readonly_structure: &mut bool,
) -> bool {
    let kind = node.kind();
    if bash_ast_node_kind_is_unsupported_for_readonly(kind) {
        return false;
    }
    if matches!(
        kind,
        "pipeline" | "list" | "redirected_statement" | "binary_expression"
    ) {
        *saw_readonly_structure = true;
    }

    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .all(|child| bash_ast_node_is_supported_readonly_candidate(child, saw_readonly_structure))
}

fn bash_ast_node_kind_is_unsupported_for_readonly(kind: &str) -> bool {
    matches!(
        kind,
        "if_statement"
            | "for_statement"
            | "while_statement"
            | "case_statement"
            | "function_definition"
            | "subshell"
            | "command_substitution"
            | "process_substitution"
            | "heredoc_redirect"
            | "heredoc_body"
            | "variable_assignment"
            | "expansion"
            | "arithmetic_expansion"
    )
}

pub(crate) fn bash_command_is_safe_readonly(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return false;
    }

    let tokens = tokenize_shell_subject_words(trimmed);
    if tokens.is_empty() {
        return false;
    }

    if for_in_file_test_echo_loop_is_safe_readonly(&tokens) {
        return true;
    }

    if tokens_contain_unsupported_readonly_expansion(&tokens) {
        return false;
    }

    let mut segment_count = 0usize;
    let mut segment = Vec::new();
    for token in &tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if !segment.is_empty() {
                segment_count = segment_count.saturating_add(1);
            }
            segment.clear();
        } else {
            segment.push(token.clone());
        }
    }
    if !segment.is_empty() {
        segment_count = segment_count.saturating_add(1);
    }
    let allow_noop_segments = segment_count > 1;

    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if !bash_segment_is_safe_readonly_with_context(&segment, allow_noop_segments) {
                return false;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    bash_segment_is_safe_readonly_with_context(&segment, allow_noop_segments)
}

#[cfg(test)]
pub(crate) fn contains_unsupported_safe_shell_syntax(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '>' | '<' | '$' | '`' | '(' | ')' | '*' | '?' | '[' | ']'
        )
    })
}

pub(crate) fn bash_segment_is_safe_readonly(words: &[String]) -> bool {
    let pipeline = split_shell_pipeline(words);
    if pipeline.len() > 1 {
        let Some((primary, filters)) = pipeline.split_first() else {
            return false;
        };
        return bash_simple_segment_is_safe_readonly(primary)
            && filters
                .iter()
                .all(|filter| shell_segment_is_read_filter(filter));
    }
    bash_simple_segment_is_safe_readonly(words)
}

fn bash_segment_is_safe_readonly_with_context(words: &[String], allow_noop: bool) -> bool {
    bash_segment_is_safe_readonly(words) || allow_noop && shell_segment_is_safe_readonly_noop(words)
}

fn bash_simple_segment_is_safe_readonly(words: &[String]) -> bool {
    let Some(command) = words.first().map(String::as_str) else {
        return false;
    };

    if !shell_segment_redirections_are_readonly(words) {
        return false;
    }

    if is_help_or_version_query(words) {
        return true;
    }

    match command {
        "pwd" | "ls" | "cat" | "head" | "tail" | "wc" | "stat" | "du" | "file" | "readlink"
        | "realpath" | "basename" | "dirname" | "diff" | "cmp" | "grep" | "rg" | "which"
        | "uname" | "date" | "whoami" | "id" => true,
        "command" => matches!(words.get(1).map(String::as_str), Some("-v")) && words.len() >= 3,
        "find" => find_segment_is_safe_readonly(words),
        "git" => git_segment_is_safe_readonly(words),
        _ => false,
    }
}

fn shell_segment_is_safe_readonly_noop(words: &[String]) -> bool {
    let Some((command, _args)) = shell_segment_command_and_args(words) else {
        return false;
    };
    if !shell_segment_redirections_are_readonly(words) {
        return false;
    }
    matches!(command, "echo" | "printf" | "true" | ":")
}

fn shell_segment_redirections_are_readonly(words: &[String]) -> bool {
    let mut index = 0usize;
    while index < words.len() {
        let word = &words[index];
        if is_fd_duplication_token(word) {
            index += 1;
            continue;
        }
        if let Some(target) = output_redirection_target(word) {
            if !shell_requested_path_is_safe_device(target) {
                return false;
            }
            index += 1;
            continue;
        }
        if is_output_redirection_operator(word) {
            let Some(target) = words.get(index + 1).map(String::as_str) else {
                return false;
            };
            if !target.starts_with('&') && !shell_requested_path_is_safe_device(target) {
                return false;
            }
            index += 2;
            continue;
        }
        if matches!(word.as_str(), "<<" | "<<-") {
            return false;
        }
        if let Some(target) = input_redirection_target(word)
            && target.starts_with('(')
        {
            return false;
        }
        index += 1;
    }
    true
}

fn output_redirection_target(word: &str) -> Option<&str> {
    [">>", ">", "1>>", "1>", "2>>", "2>", "&>>", "&>"]
        .iter()
        .find_map(|prefix| {
            word.strip_prefix(prefix)
                .filter(|target| !target.is_empty() && !target.starts_with('&'))
        })
}

fn input_redirection_target(word: &str) -> Option<&str> {
    word.strip_prefix('<')
        .filter(|target| !target.is_empty() && !target.starts_with('<'))
}

fn is_output_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | "1>" | "1>>" | "2>" | "2>>" | "&>" | "&>>"
    )
}

fn tokens_contain_unsupported_readonly_expansion(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        token.contains('$')
            || token.contains('`')
            || token.contains('*')
            || token.contains('?')
            || token.contains('(')
            || token.contains(')')
            || token.contains('[')
            || token.contains(']')
    })
}

fn for_in_file_test_echo_loop_is_safe_readonly(tokens: &[String]) -> bool {
    let Some(variable) = parse_for_in_file_test_echo_loop_variable(tokens) else {
        return false;
    };
    tokens.iter().skip(3).any(|token| token.contains('/'))
        && tokens.iter().all(|token| {
            !token.contains('`')
                && !token.contains('*')
                && !token.contains('?')
                && !token.contains('(')
                && !token.contains(')')
                && token_only_references_loop_variable(token, variable)
        })
}

fn token_only_references_loop_variable(token: &str, variable: &str) -> bool {
    let needle = format!("${variable}");
    let mut rest = token;
    while let Some(index) = rest.find('$') {
        if !rest[index..].starts_with(&needle) {
            return false;
        }
        rest = &rest[index + needle.len()..];
    }
    true
}

fn parse_for_in_file_test_echo_loop_variable(tokens: &[String]) -> Option<&str> {
    if tokens.len() < 16
        || tokens.first().map(String::as_str) != Some("for")
        || tokens.get(2).map(String::as_str) != Some("in")
    {
        return None;
    }
    let variable = tokens.get(1)?.as_str();
    if !is_shell_identifier(variable) {
        return None;
    }
    let mut cursor = 3usize;
    while tokens.get(cursor).is_some_and(|token| token != ";") {
        if tokens
            .get(cursor)
            .is_some_and(|token| token.starts_with('-'))
        {
            return None;
        }
        cursor += 1;
    }
    let variable_ref = format!("${variable}");
    let expected = [
        ";",
        "do",
        "if",
        "[",
        "-f",
        variable_ref.as_str(),
        "]",
        ";",
        "then",
        "echo",
    ];
    for expected_token in expected {
        if tokens.get(cursor).map(String::as_str) != Some(expected_token) {
            return None;
        }
        cursor += 1;
    }
    while tokens.get(cursor).is_some_and(|token| token != ";") {
        cursor += 1;
    }
    if tokens.get(cursor).map(String::as_str) != Some(";")
        || tokens.get(cursor + 1).map(String::as_str) != Some("else")
        || tokens.get(cursor + 2).map(String::as_str) != Some("echo")
    {
        return None;
    }
    cursor += 3;
    while tokens.get(cursor).is_some_and(|token| token != ";") {
        cursor += 1;
    }
    if tokens.get(cursor).map(String::as_str) != Some(";")
        || tokens.get(cursor + 1).map(String::as_str) != Some("fi")
        || tokens.get(cursor + 2).map(String::as_str) != Some(";")
        || tokens.get(cursor + 3).map(String::as_str) != Some("done")
        || cursor + 4 != tokens.len()
    {
        return None;
    }
    Some(variable)
}

fn is_shell_identifier(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && value
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

pub(crate) fn is_help_or_version_query(words: &[String]) -> bool {
    words.len() == 2
        && matches!(
            words[1].as_str(),
            "--version" | "-V" | "--help" | "-h" | "help"
        )
}

pub(crate) fn find_segment_is_safe_readonly(words: &[String]) -> bool {
    !words.iter().skip(1).any(|word| {
        matches!(
            word.as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprintf" | "-fls"
        )
    })
}

pub(crate) fn git_segment_is_safe_readonly(words: &[String]) -> bool {
    let Some(subcommand) = words.get(1).map(String::as_str) else {
        return false;
    };
    match subcommand {
        "status" | "diff" | "log" | "show" | "blame" | "rev-parse" | "ls-files" | "grep" => true,
        "branch" => words
            .iter()
            .skip(2)
            .all(|word| matches!(word.as_str(), "--show-current" | "--list")),
        _ => false,
    }
}

pub(crate) fn bash_path_subjects(workspace_root: &Path, command: &str) -> Result<Vec<ToolSubject>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    bash_path_subjects_from_cwd(&workspace_root, &workspace_root, command)
}

pub(crate) fn bash_path_subjects_from_cwd(
    workspace_root: &Path,
    initial_cwd: &Path,
    command: &str,
) -> Result<Vec<ToolSubject>> {
    let tokens = tokenize_shell_subject_words(command);
    let mut subjects = Vec::new();
    let mut cwd = initial_cwd.to_path_buf();
    let mut segment_words = Vec::new();
    for token in tokens {
        if token == "&&" || token == "||" || token == ";" {
            collect_bash_segment_subjects(workspace_root, &mut cwd, &segment_words, &mut subjects)?;
            segment_words.clear();
        } else {
            segment_words.push(token);
        }
    }
    collect_bash_segment_subjects(workspace_root, &mut cwd, &segment_words, &mut subjects)?;
    Ok(subjects)
}

pub(crate) fn collect_bash_segment_subjects(
    workspace_root: &Path,
    cwd: &mut PathBuf,
    words: &[String],
    subjects: &mut Vec<ToolSubject>,
) -> Result<()> {
    if words.is_empty() {
        return Ok(());
    }
    if words.iter().any(|word| word == "|") {
        for pipeline_segment in split_shell_pipeline(words) {
            collect_bash_segment_subjects(workspace_root, cwd, pipeline_segment, subjects)?;
        }
        return Ok(());
    }

    let command = words[0].as_str();
    let mut index = 1usize;
    if command == "cd" {
        if let Some(target) = words.get(1).filter(|word| !word.starts_with('-')) {
            let resolved = resolve_tool_path_from_base(workspace_root, cwd, target)?;
            subjects.push(resolved_tool_path_subject(resolved.clone()));
            *cwd = resolved.canonical;
        }
        return Ok(());
    }

    while index < words.len() {
        let word = &words[index];
        if let Some(target) = redirection_target(word) {
            push_shell_path_subject(subjects, workspace_root, cwd, target)?;
        } else if command == "dd" && word.starts_with("of=") && word.len() > 3 {
            push_shell_path_subject(subjects, workspace_root, cwd, &word[3..])?;
        } else if is_redirection_operator(word) {
            if let Some(target) = words.get(index + 1) {
                push_shell_path_subject(subjects, workspace_root, cwd, target)?;
                index += 1;
            }
        } else if is_path_argument(command, word) {
            push_shell_path_subject(subjects, workspace_root, cwd, word)?;
        }
        index += 1;
    }
    Ok(())
}

fn push_shell_path_subject(
    subjects: &mut Vec<ToolSubject>,
    workspace_root: &Path,
    cwd: &Path,
    requested: &str,
) -> Result<()> {
    if shell_requested_path_is_safe_device(requested) {
        return Ok(());
    }
    subjects.push(shell_path_subject(workspace_root, cwd, requested)?);
    Ok(())
}

fn shell_requested_path_is_safe_device(requested: &str) -> bool {
    matches!(requested, "/dev/null" | "/dev/stdout" | "/dev/stderr")
}

pub(crate) fn shell_path_subject(
    workspace_root: &Path,
    cwd: &Path,
    requested: &str,
) -> Result<ToolSubject> {
    resolve_tool_path_from_base(workspace_root, cwd, requested).map(resolved_tool_path_subject)
}

pub(crate) fn resolved_tool_path_subject(resolved: ResolvedToolPath) -> ToolSubject {
    ToolSubject::path_with_scope(
        resolved.original,
        resolved.normalized,
        Some(resolved.canonical),
        resolved.scope,
    )
}

pub(crate) fn tokenize_shell_subject_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote = None::<char>;
    while let Some(ch) = chars.next() {
        if quote.is_some() {
            if Some(ch) == quote {
                quote = None;
            } else if ch == '\\' {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            } else {
                current.push(ch);
            }
            continue;
        }

        match ch {
            '\'' | '"' => quote = Some(ch),
            '\\' => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' | '\t' | '\n' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            ';' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push(";".to_owned());
            }
            '&' if chars.peek() == Some(&'&') => {
                chars.next();
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("&&".to_owned());
            }
            '|' if chars.peek() == Some(&'|') => {
                chars.next();
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("||".to_owned());
            }
            '|' => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                words.push("|".to_owned());
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

pub(crate) fn is_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | "<" | "<<" | "2>" | "2>>" | "&>" | "&>>" | "1>" | "1>>"
    )
}

pub(crate) fn redirection_target(word: &str) -> Option<&str> {
    for prefix in [">>", ">", "<", "2>>", "2>", "&>>", "&>", "1>>", "1>"] {
        if let Some(target) = word
            .strip_prefix(prefix)
            .filter(|target| !target.is_empty() && !target.starts_with('&'))
        {
            return Some(target);
        }
    }
    None
}

pub(crate) fn is_path_argument(command: &str, word: &str) -> bool {
    if word.starts_with('-') || word.contains("://") {
        return false;
    }
    if word.starts_with('/')
        || word.starts_with("./")
        || word.starts_with("../")
        || word == "."
        || word == ".."
        || word.contains('/')
    {
        return true;
    }
    matches!(
        command,
        "cat"
            | "head"
            | "tail"
            | "wc"
            | "stat"
            | "du"
            | "file"
            | "readlink"
            | "realpath"
            | "basename"
            | "dirname"
            | "diff"
            | "cmp"
            | "ls"
            | "find"
            | "rm"
            | "rmdir"
            | "truncate"
            | "dd"
    )
}
