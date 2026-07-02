use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    TerminalTaskEntry, TerminalTaskId, TerminalTaskStatus, Tool, ToolAccess, ToolCategory,
    ToolContext, ToolErrorKind, ToolOperation, ToolPreviewCapability, ToolProgressEvent,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
};
use tokio::time::sleep;

use crate::{
    constants::{
        DEFAULT_TERMINAL_READ_LIMIT_BYTES, HARD_TERMINAL_READ_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV,
    },
    path::{
        absolute_path_from, canonical_workspace_root, resolve_tool_path_from_base,
        tool_path_subject,
    },
    shell::{
        ShellCommandAnalysis, analyze_shell_command, bash_path_subjects_from_cwd,
        command_permission_subject, shell_grant_scope_detail, terminal_input_permission_operation,
    },
    support::{optional_string, optional_usize, required_string},
    terminal_process::{
        MAX_TERMINAL_INPUT_BYTES, TerminalExecutionConfig, TerminalInputResult,
        TerminalProcessManager, TerminalPtySize, TerminalReadResult, TerminalResizeResult,
        TerminalStartRequest, TerminalTaskPermissionContext,
    },
};

const FOREGROUND_TERMINAL_POLL_INTERVAL_MS: u64 = 500;
const FOREGROUND_TERMINAL_PROGRESS_LIMIT_BYTES: usize = 12 * 1024;

pub(crate) struct TerminalStartTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
    pub(crate) scratch_root: PathBuf,
    pub(crate) scratch_label: String,
}
pub(crate) struct TerminalReadTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}
pub(crate) struct TerminalInputTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}
pub(crate) struct TerminalResizeTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}
pub(crate) struct TerminalCancelTool {
    pub(crate) managers: Arc<TerminalProcessManagers>,
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}

#[derive(Default)]
pub(crate) struct TerminalProcessManagers {
    terminal_execution_config: TerminalExecutionConfig,
    managers: StdMutex<BTreeMap<(PathBuf, PathBuf), Arc<TerminalProcessManager>>>,
}

impl TerminalProcessManagers {
    pub(crate) fn new(terminal_execution_config: TerminalExecutionConfig) -> Self {
        Self {
            terminal_execution_config,
            managers: StdMutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn manager_for(
        &self,
        workspace_root: &Path,
        artifact_root: &Path,
        artifact_label_root: &Path,
    ) -> Result<Arc<TerminalProcessManager>> {
        let workspace_root = canonical_workspace_root(workspace_root)?;
        let artifact_root = absolute_path_from(&workspace_root, artifact_root);
        let key = (workspace_root.clone(), artifact_root.clone());
        let mut managers = self
            .managers
            .lock()
            .map_err(|_| anyhow!("terminal process manager registry lock poisoned"))?;
        if let Some(manager) = managers.get(&key) {
            return Ok(Arc::clone(manager));
        }

        let manager = Arc::new(
            TerminalProcessManager::new_with_artifact_root_and_terminal_execution(
                &workspace_root,
                artifact_root,
                artifact_label_root.to_path_buf(),
                self.terminal_execution_config.clone(),
            )?,
        );
        managers.insert(key, Arc::clone(&manager));
        Ok(manager)
    }
}

#[async_trait]
impl Tool for TerminalStartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_start".to_owned(),
            description: format!(
                "Start a terminal task from the workspace. Use mode=foreground for one-shot checks that should return a single final result, mode=background for long-lived tasks, and pty=true/mode=interactive for tasks that need input. Use ${SIGIL_SCRATCH_DIR_ENV} for temporary shell files (shown as {}); OS temp directories are outside the workspace and require permission.external_directory.",
                self.scratch_label
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "command": { "type": "string" },
                    "cwd": { "type": "string" },
                    "shell": { "type": "string" },
                    "mode": {
                        "type": "string",
                        "enum": ["foreground", "background", "interactive"]
                    },
                    "pty": { "type": "boolean" },
                    "rows": { "type": "integer" },
                    "cols": { "type": "integer" }
                },
                "required": ["command"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let command = required_string(args, "command")?;
        let cwd = optional_string(args, "cwd");
        let shell = optional_string(args, "shell");
        let mut subjects = analyze_shell_command(&ctx.workspace_root, command)?.subjects;
        if let Some(shell) = shell {
            subjects.push(ToolSubject::command(
                shell.to_owned(),
                command_permission_subject(shell),
            ));
        }
        subjects.push(tool_path_subject(&ctx.workspace_root, cwd.unwrap_or("."))?);
        subjects.extend(terminal_command_path_subjects(
            &ctx.workspace_root,
            cwd,
            command,
        )?);
        Ok(subjects)
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
        let args = parse_terminal_start_args(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        let scratch_root = absolute_path_from(&ctx.workspace_root, &self.scratch_root);
        tokio::fs::create_dir_all(&scratch_root)
            .await
            .with_context(|| format!("failed to create {}", self.scratch_label))?;
        let analysis = analyze_shell_command(&ctx.workspace_root, &args.command)?;
        let execution_mode = resolve_terminal_start_execution_mode(args.mode, args.pty, &analysis)?;
        let mut env = BTreeMap::new();
        env.insert(
            SIGIL_SCRATCH_DIR_ENV.to_owned(),
            scratch_root.to_string_lossy().into_owned(),
        );
        let request = TerminalStartRequest {
            task_id: args.task_id,
            command: args.command,
            cwd: args.cwd,
            shell: args.shell,
            env,
        };
        let entry = if args.pty {
            manager.start_pty(request, args.pty_size).await?
        } else {
            manager.start(request).await?
        };
        if execution_mode == TerminalStartExecutionMode::Foreground {
            return wait_for_foreground_terminal(
                &ctx,
                manager,
                call_id,
                self.spec().name,
                entry,
                &analysis,
                execution_mode,
            )
            .await;
        }
        Ok(terminal_entry_result_with_shell_analysis(
            call_id,
            self.spec().name,
            "started",
            entry,
            Some(&analysis),
        ))
    }
}

#[async_trait]
impl Tool for TerminalReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_read".to_owned(),
            description: "Read a bounded slice of a terminal task output log.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "offset": { "type": "integer" },
                    "limit_bytes": { "type": "integer" }
                },
                "required": ["task_id"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Read,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let task_id = required_terminal_task_id(args)?;
        Ok(vec![terminal_task_subject(&task_id)])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let limit_bytes = terminal_read_limit(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        let read = manager.read(&task_id, offset, limit_bytes).await?;
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            read.content.clone(),
            ToolResultMeta {
                bytes: Some(read.total_bytes),
                truncated: read.truncated,
                limit_bytes: Some(limit_bytes as u64),
                returned_bytes: Some(read.returned_bytes),
                total_bytes: Some(read.total_bytes),
                returned_lines: Some(read.content.lines().count() as u64),
                details: terminal_read_details(&read, limit_bytes),
                ..ToolResultMeta::default()
            },
        ))
    }
}

#[async_trait]
impl Tool for TerminalInputTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_input".to_owned(),
            description:
                "Send input to an interactive terminal task when the backend supports stdin."
                    .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "input": {
                        "type": "string",
                        "maxLength": MAX_TERMINAL_INPUT_BYTES
                    }
                },
                "required": ["task_id", "input"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let task_id = required_terminal_task_id(args)?;
        let input = required_string(args, "input")?;
        validate_terminal_input_len(input)?;
        let context = self.terminal_input_permission_context(ctx, &task_id)?;
        let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
        let mut subjects = vec![
            terminal_task_subject(&task_id),
            terminal_input_subject(input.len()),
        ];
        subjects.extend(bash_path_subjects_from_cwd(
            &workspace_root,
            &context.cwd,
            input,
        )?);
        Ok(subjects)
    }

    fn permission_operation(&self, ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let task_id = required_terminal_task_id(args)?;
        let input = required_string(args, "input")?;
        validate_terminal_input_len(input)?;
        let _context = self.terminal_input_permission_context(ctx, &task_id)?;
        Ok(terminal_input_permission_operation(input))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let input = required_string(&args, "input")?;
        if let Err(error) = validate_terminal_input_len(input) {
            let details = json!({
                "task_id": task_id.as_str(),
                "input_bytes": input.len(),
                "limit_bytes": MAX_TERMINAL_INPUT_BYTES
            });
            let mut result = ToolResult::error(
                call_id,
                self.spec().name,
                ToolErrorKind::InvalidInput,
                error.to_string(),
            )
            .with_error_details(false, details.clone());
            result.metadata = ToolResultMeta {
                bytes: Some(input.len() as u64),
                limit_bytes: Some(MAX_TERMINAL_INPUT_BYTES as u64),
                details,
                ..ToolResultMeta::default()
            };
            return Ok(result);
        }
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        match manager.input(&task_id, input.to_owned()).await {
            Ok(result) => Ok(terminal_input_result(call_id, self.spec().name, result)),
            Err(error) if is_terminal_backend_unsupported(&error) => {
                let details = json!({
                    "task_id": task_id.as_str(),
                    "input_bytes": input.len(),
                    "supported": false,
                    "backend": "process"
                });
                let mut result = ToolResult::error(
                    call_id,
                    self.spec().name,
                    ToolErrorKind::Unsupported,
                    "terminal_input is not supported by this terminal task backend",
                )
                .with_error_details(false, details.clone());
                result.metadata = ToolResultMeta {
                    bytes: Some(input.len() as u64),
                    details,
                    ..ToolResultMeta::default()
                };
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }
}

impl TerminalInputTool {
    fn terminal_input_permission_context(
        &self,
        ctx: &ToolContext,
        task_id: &TerminalTaskId,
    ) -> Result<TerminalTaskPermissionContext> {
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        manager.permission_context(task_id)
    }
}

#[async_trait]
impl Tool for TerminalResizeTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_resize".to_owned(),
            description: "Resize a PTY-backed terminal task.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "rows": { "type": "integer" },
                    "cols": { "type": "integer" }
                },
                "required": ["task_id", "rows", "cols"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let task_id = required_terminal_task_id(args)?;
        Ok(vec![terminal_task_subject(&task_id)])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let size = required_terminal_pty_size(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        match manager.resize(&task_id, size).await {
            Ok(result) => Ok(terminal_resize_result(call_id, self.spec().name, result)),
            Err(error) if is_terminal_backend_unsupported(&error) => {
                let details = json!({
                    "task_id": task_id.as_str(),
                    "rows": size.rows,
                    "cols": size.cols,
                    "supported": false,
                    "backend": "process"
                });
                let mut result = ToolResult::error(
                    call_id,
                    self.spec().name,
                    ToolErrorKind::Unsupported,
                    "terminal_resize is not supported by this terminal task backend",
                )
                .with_error_details(false, details.clone());
                result.metadata = ToolResultMeta {
                    details,
                    ..ToolResultMeta::default()
                };
                Ok(result)
            }
            Err(error) => Err(error),
        }
    }
}

#[async_trait]
impl Tool for TerminalCancelTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_cancel".to_owned(),
            description: "Cancel a running terminal task with terminate and kill fallback."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" }
                },
                "required": ["task_id"]
            }),
            category: ToolCategory::Shell,
            access: ToolAccess::Execute,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let task_id = required_terminal_task_id(args)?;
        Ok(vec![terminal_task_subject(&task_id)])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let task_id = required_terminal_task_id(&args)?;
        let manager = self.managers.manager_for(
            &ctx.workspace_root,
            &self.artifact_root,
            &self.artifact_label_root,
        )?;
        let entry = manager.cancel(&task_id).await?;
        Ok(terminal_entry_result(
            call_id,
            self.spec().name,
            "cancelled",
            entry,
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerminalStartExecutionMode {
    Foreground,
    Background,
    Interactive,
}

impl TerminalStartExecutionMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Background => "background",
            Self::Interactive => "interactive",
        }
    }

    fn parse(value: &str) -> Result<Self> {
        match value {
            "foreground" => Ok(Self::Foreground),
            "background" => Ok(Self::Background),
            "interactive" => Ok(Self::Interactive),
            _ => bail!("terminal_start mode must be foreground, background, or interactive"),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TerminalStartArgs {
    task_id: Option<TerminalTaskId>,
    command: String,
    cwd: Option<PathBuf>,
    shell: Option<String>,
    mode: Option<TerminalStartExecutionMode>,
    pty: bool,
    pty_size: Option<TerminalPtySize>,
}

pub(crate) fn parse_terminal_start_args(args: &Value) -> Result<TerminalStartArgs> {
    let task_id = optional_string(args, "task_id")
        .map(|task_id| TerminalTaskId::new(task_id.to_owned()))
        .transpose()?;
    let command = required_string(args, "command")?.to_owned();
    let cwd = optional_string(args, "cwd").map(PathBuf::from);
    let shell = optional_string(args, "shell").map(str::to_owned);
    let mode = optional_string(args, "mode")
        .map(TerminalStartExecutionMode::parse)
        .transpose()?;
    let pty = args.get("pty").and_then(Value::as_bool).unwrap_or(false);
    let pty_size = if args.get("rows").is_some() || args.get("cols").is_some() {
        Some(required_terminal_pty_size(args)?)
    } else {
        None
    };
    Ok(TerminalStartArgs {
        task_id,
        command,
        cwd,
        shell,
        mode,
        pty,
        pty_size,
    })
}

fn resolve_terminal_start_execution_mode(
    requested: Option<TerminalStartExecutionMode>,
    pty: bool,
    analysis: &ShellCommandAnalysis,
) -> Result<TerminalStartExecutionMode> {
    let mode = requested.unwrap_or_else(|| {
        if pty {
            TerminalStartExecutionMode::Interactive
        } else if analysis.command_family.is_workspace_check() {
            TerminalStartExecutionMode::Foreground
        } else {
            TerminalStartExecutionMode::Background
        }
    });
    match (mode, pty) {
        (TerminalStartExecutionMode::Foreground, true) => {
            bail!("terminal_start mode=foreground does not support pty=true")
        }
        (TerminalStartExecutionMode::Interactive, false) => {
            bail!("terminal_start mode=interactive requires pty=true")
        }
        _ => Ok(mode),
    }
}

pub(crate) fn required_terminal_task_id(args: &Value) -> Result<TerminalTaskId> {
    TerminalTaskId::new(required_string(args, "task_id")?.to_owned())
}

pub(crate) fn required_terminal_pty_size(args: &Value) -> Result<TerminalPtySize> {
    TerminalPtySize::new(required_u16(args, "rows")?, required_u16(args, "cols")?)
}

pub(crate) fn required_u16(args: &Value, key: &str) -> Result<u16> {
    let value = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing positive integer field {key}"))?;
    u16::try_from(value).map_err(|_| anyhow!("{key} is too large for a terminal dimension"))
}

pub(crate) fn terminal_read_limit(args: &Value) -> Result<usize> {
    Ok(optional_usize(args, "limit_bytes")?
        .unwrap_or(DEFAULT_TERMINAL_READ_LIMIT_BYTES)
        .clamp(1, HARD_TERMINAL_READ_LIMIT_BYTES))
}

pub(crate) fn terminal_task_subject(task_id: &TerminalTaskId) -> ToolSubject {
    let value = format!("terminal_task:{}", task_id.as_str());
    ToolSubject::command(value.clone(), value)
}

pub(crate) fn terminal_input_subject(input_bytes: usize) -> ToolSubject {
    ToolSubject::command(
        format!("terminal_input bytes={input_bytes}"),
        format!("terminal_input_bytes:{input_bytes}"),
    )
}

pub(crate) fn validate_terminal_input_len(input: &str) -> Result<()> {
    if input.len() > MAX_TERMINAL_INPUT_BYTES {
        bail!(
            "terminal_input input exceeds maximum of {} bytes",
            MAX_TERMINAL_INPUT_BYTES
        );
    }
    Ok(())
}

pub(crate) fn terminal_command_path_subjects(
    workspace_root: &Path,
    cwd: Option<&str>,
    command: &str,
) -> Result<Vec<ToolSubject>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let cwd = cwd
        .map(|cwd| resolve_tool_path_from_base(&workspace_root, &workspace_root, cwd))
        .transpose()?
        .map(|resolved| resolved.canonical)
        .unwrap_or_else(|| workspace_root.clone());
    bash_path_subjects_from_cwd(&workspace_root, &cwd, command)
}

pub(crate) fn terminal_entry_result(
    call_id: String,
    tool_name: String,
    action: &'static str,
    entry: TerminalTaskEntry,
) -> ToolResult {
    terminal_entry_result_with_shell_analysis(call_id, tool_name, action, entry, None)
}

pub(crate) fn terminal_entry_result_with_shell_analysis(
    call_id: String,
    tool_name: String,
    action: &'static str,
    entry: TerminalTaskEntry,
    analysis: Option<&ShellCommandAnalysis>,
) -> ToolResult {
    let content = format!(
        "{action} terminal task {}\nstatus: {}\nlog: {}",
        entry.handle.task_id.as_str(),
        entry.status.as_str(),
        entry.handle.log_path.display()
    );
    ToolResult::ok(
        call_id,
        tool_name,
        content,
        ToolResultMeta {
            truncated: entry.output_truncated,
            details: terminal_entry_details(&entry, analysis),
            ..ToolResultMeta::default()
        },
    )
}

async fn wait_for_foreground_terminal(
    ctx: &ToolContext,
    manager: Arc<TerminalProcessManager>,
    call_id: String,
    tool_name: String,
    entry: TerminalTaskEntry,
    analysis: &ShellCommandAnalysis,
    execution_mode: TerminalStartExecutionMode,
) -> Result<ToolResult> {
    let started = Instant::now();
    let timeout_after = Duration::from_secs(ctx.timeout_secs.max(1));
    let task_id = entry.handle.task_id.clone();
    let read_limit =
        FOREGROUND_TERMINAL_PROGRESS_LIMIT_BYTES.clamp(1, HARD_TERMINAL_READ_LIMIT_BYTES);
    let mut latest = entry;
    let mut next_offset = 0;
    let mut sequence = 0;

    emit_terminal_progress(
        ctx,
        &call_id,
        &tool_name,
        sequence,
        &latest,
        analysis,
        execution_mode,
        None,
        None,
        None,
    )?;

    loop {
        if latest.status.is_terminal() {
            return Ok(terminal_foreground_result(
                call_id,
                tool_name,
                latest,
                analysis,
                execution_mode,
                elapsed_millis(started),
                None,
                None,
            ));
        }

        if started.elapsed() >= timeout_after {
            let cancelled = manager
                .cancel(&task_id)
                .await
                .unwrap_or_else(|_| latest.clone());
            sequence = sequence.saturating_add(1);
            let duration_ms = elapsed_millis(started);
            emit_terminal_progress(
                ctx,
                &call_id,
                &tool_name,
                sequence,
                &cancelled,
                analysis,
                execution_mode,
                None,
                None,
                Some("timed_out"),
            )?;
            return Ok(terminal_foreground_result(
                call_id,
                tool_name,
                cancelled,
                analysis,
                execution_mode,
                duration_ms,
                Some("timed_out"),
                Some(ToolErrorKind::Timeout),
            ));
        }

        sleep(Duration::from_millis(FOREGROUND_TERMINAL_POLL_INTERVAL_MS)).await;
        let previous_status = latest.status.clone();
        let read = manager.read(&task_id, next_offset, read_limit).await?;
        next_offset = read.next_offset.unwrap_or(read.total_bytes);
        if let Some(entry) = read.latest_entry.clone() {
            latest = entry;
        }
        if read.returned_bytes > 0
            || latest.status != previous_status
            || latest.status.is_terminal()
        {
            sequence = sequence.saturating_add(1);
            emit_terminal_progress(
                ctx,
                &call_id,
                &tool_name,
                sequence,
                &latest,
                analysis,
                execution_mode,
                (!read.content.is_empty()).then_some(read.content.as_str()),
                Some(read.total_bytes),
                None,
            )?;
        }
    }
}

fn emit_terminal_progress(
    ctx: &ToolContext,
    call_id: &str,
    tool_name: &str,
    sequence: u64,
    entry: &TerminalTaskEntry,
    analysis: &ShellCommandAnalysis,
    execution_mode: TerminalStartExecutionMode,
    output_preview: Option<&str>,
    total_bytes: Option<u64>,
    verdict_override: Option<&str>,
) -> Result<()> {
    ctx.emit_progress(ToolProgressEvent {
        execution_id: entry.handle.task_id.as_str().to_owned(),
        call_id: call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        sequence,
        status: entry.status.as_str().to_owned(),
        message: Some(format!(
            "terminal {} {}",
            entry.handle.task_id.as_str(),
            entry.status.as_str()
        )),
        output_preview: output_preview.map(str::to_owned),
        output_log_ref: Some(entry.handle.log_path.clone()),
        total_bytes,
        updated_at_ms: Some(entry.updated_at_ms),
        details: terminal_entry_details_with_execution_mode(
            entry,
            Some(analysis),
            execution_mode,
            verdict_override,
        ),
    })
}

fn terminal_foreground_result(
    call_id: String,
    tool_name: String,
    entry: TerminalTaskEntry,
    analysis: &ShellCommandAnalysis,
    execution_mode: TerminalStartExecutionMode,
    duration_ms: u64,
    verdict_override: Option<&str>,
    error_kind_override: Option<ToolErrorKind>,
) -> ToolResult {
    let exit_code = terminal_exit_code(&entry.status);
    let verdict = verdict_override.unwrap_or_else(|| terminal_verdict(&entry.status));
    let rerun_not_needed = matches!(verdict, "passed" | "failed");
    let mut details =
        terminal_entry_details_with_execution_mode(&entry, Some(analysis), execution_mode, None);
    set_terminal_details_verdict(&mut details, verdict, rerun_not_needed);
    if let Some(object) = details.as_object_mut() {
        object.insert("duration_ms".to_owned(), json!(duration_ms));
        object.insert("output_log_ref".to_owned(), json!(&entry.handle.log_path));
    }

    let content = terminal_foreground_content(&entry, verdict, duration_ms);
    let metadata = ToolResultMeta {
        duration_ms: Some(duration_ms),
        exit_code,
        truncated: entry.output_truncated,
        returned_bytes: Some(content.len() as u64),
        returned_lines: Some(content.lines().count() as u64),
        details: details.clone(),
        ..ToolResultMeta::default()
    };

    if let Some(error_kind) = error_kind_override.or_else(|| terminal_error_kind(&entry.status)) {
        let mut result = ToolResult::error(call_id, tool_name, error_kind, content)
            .with_error_details(false, details);
        result.metadata = metadata;
        return result;
    }

    ToolResult::ok(call_id, tool_name, content, metadata)
}

fn terminal_foreground_content(
    entry: &TerminalTaskEntry,
    verdict: &str,
    duration_ms: u64,
) -> String {
    let mut lines = vec![format!(
        "terminal task {} {} · verdict {} · {} ms",
        entry.handle.task_id.as_str(),
        entry.status.as_str(),
        verdict,
        duration_ms
    )];
    if let Some(code) = terminal_exit_code(&entry.status) {
        lines.push(format!("exit_code: {code}"));
    }
    lines.push(format!("log: {}", entry.handle.log_path.display()));
    if entry.output_truncated {
        lines.push("output_truncated: true".to_owned());
    }
    if let Some(preview) = entry
        .output_preview
        .as_deref()
        .filter(|preview| !preview.is_empty())
    {
        lines.push(String::new());
        lines.push(preview.to_owned());
    }
    lines.join("\n")
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn terminal_exit_code(status: &TerminalTaskStatus) -> Option<i32> {
    match status {
        TerminalTaskStatus::Exited { exit_code } => *exit_code,
        _ => None,
    }
}

fn terminal_verdict(status: &TerminalTaskStatus) -> &'static str {
    match status {
        TerminalTaskStatus::Exited { exit_code: Some(0) } => "passed",
        TerminalTaskStatus::Exited { .. } | TerminalTaskStatus::Failed { .. } => "failed",
        TerminalTaskStatus::Cancelled => "cancelled",
        TerminalTaskStatus::Interrupted => "interrupted",
        TerminalTaskStatus::Starting | TerminalTaskStatus::Running => "running",
    }
}

fn terminal_error_kind(status: &TerminalTaskStatus) -> Option<ToolErrorKind> {
    match status {
        TerminalTaskStatus::Exited { exit_code: Some(0) } => None,
        TerminalTaskStatus::Exited { .. } => Some(ToolErrorKind::ExitStatus),
        TerminalTaskStatus::Failed { .. } => Some(ToolErrorKind::Internal),
        TerminalTaskStatus::Cancelled => Some(ToolErrorKind::Interrupted),
        TerminalTaskStatus::Interrupted => Some(ToolErrorKind::Interrupted),
        TerminalTaskStatus::Starting | TerminalTaskStatus::Running => None,
    }
}

pub(crate) fn terminal_entry_details(
    entry: &TerminalTaskEntry,
    analysis: Option<&ShellCommandAnalysis>,
) -> Value {
    let mut details = json!({
        "task_id": entry.handle.task_id.as_str(),
        "status": entry.status.as_str(),
        "status_detail": &entry.status,
        "command": &entry.handle.command,
        "cwd": &entry.handle.cwd,
        "shell": &entry.handle.shell,
        "log_path": &entry.handle.log_path,
        "created_at_ms": entry.handle.created_at_ms,
        "updated_at_ms": entry.updated_at_ms,
        "output_preview": &entry.output_preview,
        "output_hash": &entry.output_hash,
        "output_truncated": entry.output_truncated
    });
    let details_object = details
        .as_object_mut()
        .expect("terminal task details should be a JSON object");
    details_object.insert(
        "execution_backend".to_owned(),
        json!(entry.handle.execution_backend),
    );
    details_object.insert(
        "execution_backend_capabilities".to_owned(),
        json!(entry.handle.execution_backend_capabilities),
    );
    details_object.insert(
        "enforcement_backend".to_owned(),
        json!(entry.handle.enforcement_backend),
    );
    details_object.insert(
        "enforcement_backend_capabilities".to_owned(),
        json!(entry.handle.enforcement_backend_capabilities),
    );
    details_object.insert(
        "sandbox_profile".to_owned(),
        json!(entry.handle.sandbox_profile),
    );
    details_object.insert("cleanup".to_owned(), json!(entry.cleanup));
    if let Some(analysis) = analysis {
        details_object.insert(
            "shell_analysis".to_owned(),
            json!({
                "command": analysis.command.as_str(),
                "command_family": analysis.command_family.as_str(),
                "grant_scope": analysis.grant_scope.as_ref().map(|scope| scope.as_str()),
                "grant_scope_detail": shell_grant_scope_detail(analysis.grant_scope.as_ref()),
                "approval_reason": analysis.explanation.as_str(),
                "exit_code": Value::Null,
                "verdict": "running",
                "output_truncated": entry.output_truncated,
                "tail_available": false,
                "rerun_not_needed": false,
            }),
        );
    }
    details
}

fn terminal_entry_details_with_execution_mode(
    entry: &TerminalTaskEntry,
    analysis: Option<&ShellCommandAnalysis>,
    execution_mode: TerminalStartExecutionMode,
    verdict_override: Option<&str>,
) -> Value {
    let mut details = terminal_entry_details(entry, analysis);
    let verdict = verdict_override.unwrap_or_else(|| terminal_verdict(&entry.status));
    let rerun_not_needed = matches!(verdict, "passed" | "failed");
    if let Some(object) = details.as_object_mut() {
        object.insert(
            "execution_id".to_owned(),
            json!(entry.handle.task_id.as_str()),
        );
        object.insert("execution_mode".to_owned(), json!(execution_mode.as_str()));
        object.insert(
            "exit_code".to_owned(),
            json!(terminal_exit_code(&entry.status)),
        );
        object.insert("verdict".to_owned(), json!(verdict));
        object.insert("output_log_ref".to_owned(), json!(&entry.handle.log_path));
        object.insert("tail_available".to_owned(), json!(false));
        object.insert("rerun_not_needed".to_owned(), json!(rerun_not_needed));
        if let Some(shell) = object
            .get_mut("shell_analysis")
            .and_then(serde_json::Value::as_object_mut)
        {
            shell.insert(
                "exit_code".to_owned(),
                json!(terminal_exit_code(&entry.status)),
            );
        }
    }
    set_terminal_details_verdict(&mut details, verdict, rerun_not_needed);
    details
}

fn set_terminal_details_verdict(details: &mut Value, verdict: &str, rerun_not_needed: bool) {
    let Some(object) = details.as_object_mut() else {
        return;
    };
    object.insert("verdict".to_owned(), json!(verdict));
    object.insert("rerun_not_needed".to_owned(), json!(rerun_not_needed));
    if let Some(shell) = object
        .get_mut("shell_analysis")
        .and_then(serde_json::Value::as_object_mut)
    {
        shell.insert("verdict".to_owned(), json!(verdict));
        shell.insert("tail_available".to_owned(), json!(false));
        shell.insert("rerun_not_needed".to_owned(), json!(rerun_not_needed));
    }
}

pub(crate) fn terminal_input_result(
    call_id: String,
    tool_name: String,
    result: TerminalInputResult,
) -> ToolResult {
    ToolResult::ok(
        call_id,
        tool_name,
        format!(
            "queued {} bytes for terminal task {}",
            result.input_bytes,
            result.task_id.as_str()
        ),
        ToolResultMeta {
            bytes: Some(result.input_bytes),
            details: json!({
                "task_id": result.task_id.as_str(),
                "input_bytes": result.input_bytes,
                "backend": result.backend.as_str(),
                "supported": true
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(crate) fn terminal_resize_result(
    call_id: String,
    tool_name: String,
    result: TerminalResizeResult,
) -> ToolResult {
    ToolResult::ok(
        call_id,
        tool_name,
        format!(
            "resized terminal task {} to {}x{}",
            result.task_id.as_str(),
            result.size.cols,
            result.size.rows
        ),
        ToolResultMeta {
            details: json!({
                "task_id": result.task_id.as_str(),
                "rows": result.size.rows,
                "cols": result.size.cols,
                "backend": result.backend.as_str(),
                "supported": true
            }),
            ..ToolResultMeta::default()
        },
    )
}

pub(crate) fn is_terminal_backend_unsupported(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("backend does not support input")
        || message.contains("backend does not support resize")
}

pub(crate) fn terminal_read_details(read: &TerminalReadResult, limit_bytes: usize) -> Value {
    let mut details = json!({
        "task_id": read.task_id.as_str(),
        "offset": read.offset,
        "next_offset": read.next_offset,
        "returned_bytes": read.returned_bytes,
        "total_bytes": read.total_bytes,
        "limit_bytes": limit_bytes,
        "truncated": read.truncated
    });
    if let Some(entry) = &read.latest_entry
        && let Some(object) = details.as_object_mut()
    {
        object.insert(
            "terminal_task".to_owned(),
            terminal_entry_details(entry, None),
        );
    }
    details
}
