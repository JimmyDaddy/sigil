use std::{collections::BTreeSet, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sigil_kernel::{
    DeclaredToolPermissionFacts, Tool, ToolAccess, ToolCapability, ToolCategory,
    ToolConcurrencyClass, ToolContext, ToolErrorKind, ToolMutationTracking, ToolOperation,
    ToolPermissionPlanDraft, ToolPreviewCapability, ToolResult, ToolResultMeta, ToolResultStatus,
    ToolSpec, declared_tool_permission_plan,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

use crate::{
    path::{canonical_workspace_root, tool_path_subject},
    support::{optional_usize, required_string},
};

const DEFAULT_ENTRY_LIMIT: usize = 200;
const HARD_ENTRY_LIMIT: usize = 1_000;
const STDOUT_CAPTURE_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const STDERR_CAPTURE_LIMIT_BYTES: usize = 64 * 1024;

pub(crate) struct VcsInspectTool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VcsInspectionOperation {
    Status,
    DiffNames,
    DiffStat,
    StagedStat,
    Unmerged,
}

impl VcsInspectionOperation {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "status" => Ok(Self::Status),
            "diff_names" => Ok(Self::DiffNames),
            "diff_stat" => Ok(Self::DiffStat),
            "staged_stat" => Ok(Self::StagedStat),
            "unmerged" => Ok(Self::Unmerged),
            _ => Err(anyhow!(
                "operation must be one of status, diff_names, diff_stat, staged_stat, unmerged"
            )),
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::DiffNames => "diff_names",
            Self::DiffStat => "diff_stat",
            Self::StagedStat => "staged_stat",
            Self::Unmerged => "unmerged",
        }
    }

    fn command_args(self) -> &'static [&'static str] {
        match self {
            Self::Status => &[
                "status",
                "--porcelain=v1",
                "--branch",
                "--untracked-files=normal",
                "--ignore-submodules=all",
            ],
            Self::DiffNames => &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
                "--name-only",
                "--",
            ],
            Self::DiffStat => &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
                "--numstat",
                "--",
            ],
            Self::StagedStat => &[
                "diff",
                "--cached",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
                "--numstat",
                "--",
            ],
            Self::Unmerged => &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--ignore-submodules=all",
                "--name-only",
                "--diff-filter=U",
                "--",
            ],
        }
    }
}

#[derive(Debug)]
struct RepositoryPaths {
    workspace_root: PathBuf,
    git_dir: PathBuf,
}

#[derive(Debug)]
struct CappedBytes {
    bytes: Vec<u8>,
    total_bytes: u64,
    truncated: bool,
}

#[derive(Debug)]
struct GitOutput {
    success: bool,
    exit_code: Option<i32>,
    stdout: CappedBytes,
    stderr: CappedBytes,
}

#[derive(Debug)]
struct VcsFailure {
    kind: ToolErrorKind,
    message: &'static str,
    retryable: bool,
    details: Value,
}

#[async_trait]
impl Tool for VcsInspectTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "vcs_inspect".to_owned(),
            description: "Inspect the workspace Git repository with one fixed read-only operation. Supports status, unstaged changed names/statistics, staged statistics, and unmerged paths; it does not accept commands, paths, revisions, or arbitrary Git arguments."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": [
                            "status",
                            "diff_names",
                            "diff_stat",
                            "staged_stat",
                            "unmerged"
                        ]
                    },
                    "limit": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": HARD_ENTRY_LIMIT
                    }
                },
                "required": ["operation"],
                "additionalProperties": false
            }),
            category: ToolCategory::Search,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::None
    }

    fn concurrency_class(&self) -> ToolConcurrencyClass {
        ToolConcurrencyClass::ParallelReadOnly
    }

    fn capabilities(&self) -> BTreeSet<ToolCapability> {
        [ToolCapability::WorkspaceRead, ToolCapability::VcsRead]
            .into_iter()
            .collect()
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let operation = required_string(args, "operation")?;
        VcsInspectionOperation::parse(operation)?;
        if let Some(limit) = optional_usize(args, "limit")?
            && limit == 0
        {
            return Err(anyhow!("limit must be a positive integer"));
        }
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Search,
                network_effect: None,
                subjects: vec![tool_path_subject(&ctx.workspace_root, ".")?],
                tool_default_mode: None,
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let operation =
            match required_string(&args, "operation").and_then(VcsInspectionOperation::parse) {
                Ok(operation) => operation,
                Err(_) => {
                    return Ok(vcs_error_result(
                        call_id,
                        ToolErrorKind::InvalidInput,
                        "vcs_inspect requires a supported fixed operation",
                        false,
                        json!({
                            "supported_operations": [
                                "status",
                                "diff_names",
                                "diff_stat",
                                "staged_stat",
                                "unmerged"
                            ]
                        }),
                    ));
                }
            };
        let limit = match optional_usize(&args, "limit") {
            Ok(Some(0)) => {
                return Ok(vcs_error_result(
                    call_id,
                    ToolErrorKind::InvalidInput,
                    "vcs_inspect limit must be a positive integer",
                    false,
                    Value::Null,
                ));
            }
            Ok(limit) => limit.unwrap_or(DEFAULT_ENTRY_LIMIT).min(HARD_ENTRY_LIMIT),
            Err(_) => {
                return Ok(vcs_error_result(
                    call_id,
                    ToolErrorKind::InvalidInput,
                    "vcs_inspect limit must be a positive integer",
                    false,
                    Value::Null,
                ));
            }
        };
        let repository = match resolve_repository_paths(&ctx) {
            Ok(repository) => repository,
            Err(failure) => {
                return Ok(vcs_error_result(
                    call_id,
                    failure.kind,
                    failure.message,
                    failure.retryable,
                    failure.details,
                ));
            }
        };
        let started = std::time::Instant::now();
        let output = match run_git_inspection(&ctx, &repository, operation).await {
            Ok(output) => output,
            Err(failure) => {
                return Ok(vcs_error_result(
                    call_id,
                    failure.kind,
                    failure.message,
                    failure.retryable,
                    failure.details,
                ));
            }
        };
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if !output.success {
            return Ok(vcs_error_result(
                call_id,
                ToolErrorKind::ExitStatus,
                "git could not inspect the workspace repository",
                true,
                json!({
                    "operation": operation.as_str(),
                    "exit_code": output.exit_code,
                    "stderr_bytes": output.stderr.total_bytes,
                    "stderr_truncated": output.stderr.truncated
                }),
            ));
        }
        let stdout_captured_bytes = output.stdout.bytes.len() as u64;
        let stdout_bytes = complete_line_prefix(output.stdout.bytes, output.stdout.truncated);
        let stdout = match String::from_utf8(stdout_bytes) {
            Ok(stdout) => stdout,
            Err(_) => {
                return Ok(vcs_error_result(
                    call_id,
                    ToolErrorKind::Utf8,
                    "git returned a path that could not be represented as UTF-8",
                    false,
                    json!({ "operation": operation.as_str() }),
                ));
            }
        };
        let payload = match inspection_payload(operation, &stdout, limit, output.stdout.truncated) {
            Ok(payload) => payload,
            Err(_) => {
                return Ok(vcs_error_result(
                    call_id,
                    ToolErrorKind::Protocol,
                    "git returned an unexpected result for the requested inspection",
                    true,
                    json!({ "operation": operation.as_str() }),
                ));
            }
        };
        let content = serde_json::to_string_pretty(&payload.value)?;
        let content_bytes = content.len() as u64;
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            content,
            ToolResultMeta {
                duration_ms: Some(duration_ms),
                exit_code: output.exit_code,
                stdout_bytes: Some(output.stdout.total_bytes),
                stderr_bytes: Some(output.stderr.total_bytes),
                bytes: Some(content_bytes),
                truncated: payload.truncated,
                omitted_bytes: output
                    .stdout
                    .total_bytes
                    .checked_sub(stdout_captured_bytes)
                    .filter(|omitted| *omitted > 0),
                limit_bytes: Some(STDOUT_CAPTURE_LIMIT_BYTES as u64),
                limit_lines: Some(limit as u64),
                returned_entries: Some(payload.returned_entries as u64),
                total_entries: payload.total_entries.map(|count| count as u64),
                details: json!({
                    "operation": operation.as_str(),
                    "repository": ".",
                    "raw_output_truncated": output.stdout.truncated,
                    "stderr_captured_bytes": output.stderr.bytes.len()
                }),
                ..ToolResultMeta::default()
            },
        ))
    }
}

fn complete_line_prefix(mut bytes: Vec<u8>, truncated: bool) -> Vec<u8> {
    if truncated && let Some(last_newline) = bytes.iter().rposition(|byte| *byte == b'\n') {
        bytes.truncate(last_newline + 1);
    }
    bytes
}

fn resolve_repository_paths(ctx: &ToolContext) -> Result<RepositoryPaths, VcsFailure> {
    let workspace_root = canonical_workspace_root(&ctx.workspace_root).map_err(|_| VcsFailure {
        kind: ToolErrorKind::NotFound,
        message: "workspace root could not be resolved",
        retryable: false,
        details: Value::Null,
    })?;
    let dot_git = workspace_root.join(".git");
    let metadata = std::fs::symlink_metadata(&dot_git).map_err(|error| VcsFailure {
        kind: if error.kind() == std::io::ErrorKind::NotFound {
            ToolErrorKind::NotFound
        } else {
            ToolErrorKind::Io
        },
        message: if error.kind() == std::io::ErrorKind::NotFound {
            "workspace root is not a Git repository"
        } else {
            "workspace Git metadata could not be inspected"
        },
        retryable: false,
        details: json!({ "repository": "." }),
    })?;
    let git_dir = if metadata.is_file() {
        let contents = std::fs::read_to_string(&dot_git).map_err(|_| VcsFailure {
            kind: ToolErrorKind::Io,
            message: "workspace Git metadata pointer could not be read",
            retryable: false,
            details: json!({ "repository": "." }),
        })?;
        let relative = contents
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .ok_or_else(|| VcsFailure {
                kind: ToolErrorKind::Protocol,
                message: "workspace Git metadata pointer is invalid",
                retryable: false,
                details: json!({ "repository": "." }),
            })?;
        let path = PathBuf::from(relative);
        let candidate = if path.is_absolute() {
            path
        } else {
            workspace_root.join(path)
        };
        std::fs::canonicalize(candidate).map_err(|_| VcsFailure {
            kind: ToolErrorKind::NotFound,
            message: "workspace Git metadata target could not be resolved",
            retryable: false,
            details: json!({ "repository": "." }),
        })?
    } else if metadata.is_dir() || metadata.file_type().is_symlink() {
        std::fs::canonicalize(&dot_git).map_err(|_| VcsFailure {
            kind: ToolErrorKind::NotFound,
            message: "workspace Git metadata target could not be resolved",
            retryable: false,
            details: json!({ "repository": "." }),
        })?
    } else {
        return Err(VcsFailure {
            kind: ToolErrorKind::Protocol,
            message: "workspace Git metadata is not a directory or pointer",
            retryable: false,
            details: json!({ "repository": "." }),
        });
    };
    if !git_dir.starts_with(&workspace_root) {
        return Err(VcsFailure {
            kind: ToolErrorKind::PathOutsideWorkspace,
            message: "workspace Git metadata resolves outside the workspace",
            retryable: false,
            details: json!({ "repository": "." }),
        });
    }
    Ok(RepositoryPaths {
        workspace_root,
        git_dir,
    })
}

async fn run_git_inspection(
    ctx: &ToolContext,
    repository: &RepositoryPaths,
    operation: VcsInspectionOperation,
) -> Result<GitOutput, VcsFailure> {
    let _process_effect = ctx
        .begin_forward_effect(sigil_kernel::RunEffectKind::Process)
        .map_err(|_| VcsFailure {
            kind: ToolErrorKind::Interrupted,
            message: "git workspace inspection was cancelled before process start",
            retryable: true,
            details: json!({ "operation": operation.as_str() }),
        })?;
    let mut command = Command::new("git");
    command
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(&repository.workspace_root)
        .arg("--no-pager")
        .arg("--no-optional-locks")
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.untrackedCache=false"])
        .args(["-c", "core.preloadIndex=false"])
        .args(["-c", "core.quotePath=true"])
        .arg("--git-dir")
        .arg(&repository.git_dir)
        .arg("--work-tree")
        .arg(&repository.workspace_root)
        .args(operation.command_args())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_PAGER", "cat")
        .env("PAGER", "cat")
        .env("LC_ALL", "C")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIFF_OPTS");
    let mut child = command.spawn().map_err(|_| VcsFailure {
        kind: ToolErrorKind::Unsupported,
        message: "git executable is not available for workspace inspection",
        retryable: false,
        details: json!({ "operation": operation.as_str() }),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| VcsFailure {
        kind: ToolErrorKind::Internal,
        message: "git stdout could not be captured",
        retryable: true,
        details: json!({ "operation": operation.as_str() }),
    })?;
    let stderr = child.stderr.take().ok_or_else(|| VcsFailure {
        kind: ToolErrorKind::Internal,
        message: "git stderr could not be captured",
        retryable: true,
        details: json!({ "operation": operation.as_str() }),
    })?;
    let wait = async move {
        let (stdout, stderr, status) = tokio::join!(
            read_capped(stdout, STDOUT_CAPTURE_LIMIT_BYTES),
            read_capped(stderr, STDERR_CAPTURE_LIMIT_BYTES),
            child.wait()
        );
        Ok::<_, std::io::Error>((stdout?, stderr?, status?))
    };
    let bounded_wait = timeout(Duration::from_secs(ctx.timeout_secs.max(1)), wait);
    let settled = if let Some(cancellation) = ctx.cancellation_handle() {
        tokio::select! {
            result = bounded_wait => result,
            () = cancellation.cancelled() => {
                return Err(VcsFailure {
                    kind: ToolErrorKind::Interrupted,
                    message: "git workspace inspection was cancelled",
                    retryable: true,
                    details: json!({ "operation": operation.as_str() }),
                });
            }
        }
    } else {
        bounded_wait.await
    };
    let (stdout, stderr, status) = settled
        .map_err(|_| VcsFailure {
            kind: ToolErrorKind::Timeout,
            message: "git workspace inspection timed out",
            retryable: true,
            details: json!({
                "operation": operation.as_str(),
                "timeout_seconds": ctx.timeout_secs.max(1)
            }),
        })?
        .map_err(|_| VcsFailure {
            kind: ToolErrorKind::Io,
            message: "git workspace inspection output could not be captured",
            retryable: true,
            details: json!({ "operation": operation.as_str() }),
        })?;
    Ok(GitOutput {
        success: status.success(),
        exit_code: status.code(),
        stdout,
        stderr,
    })
}

async fn read_capped<R>(mut reader: R, limit: usize) -> std::io::Result<CappedBytes>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut captured = Vec::with_capacity(limit.min(64 * 1024));
    let mut total_bytes = 0u64;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(read as u64);
        if captured.len() < limit {
            let remaining = limit - captured.len();
            captured.extend_from_slice(&buffer[..read.min(remaining)]);
        }
    }
    Ok(CappedBytes {
        truncated: total_bytes > captured.len() as u64,
        bytes: captured,
        total_bytes,
    })
}

struct InspectionPayload {
    value: Value,
    returned_entries: usize,
    total_entries: Option<usize>,
    truncated: bool,
}

fn inspection_payload(
    operation: VcsInspectionOperation,
    stdout: &str,
    limit: usize,
    raw_truncated: bool,
) -> Result<InspectionPayload> {
    match operation {
        VcsInspectionOperation::Status => status_payload(stdout, limit, raw_truncated),
        VcsInspectionOperation::DiffStat | VcsInspectionOperation::StagedStat => {
            stat_payload(operation, stdout, limit, raw_truncated)
        }
        VcsInspectionOperation::DiffNames | VcsInspectionOperation::Unmerged => {
            path_payload(operation, stdout, limit, raw_truncated)
        }
    }
}

fn status_payload(stdout: &str, limit: usize, raw_truncated: bool) -> Result<InspectionPayload> {
    let mut branch = None;
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("## ") {
            branch = Some(value.to_owned());
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.len() < 3 || bytes[2] != b' ' {
            return Err(anyhow!("unexpected porcelain status record"));
        }
        entries.push(json!({
            "status": &line[..2],
            "path": &line[3..]
        }));
    }
    finalize_payload("status", branch, entries, limit, raw_truncated)
}

fn path_payload(
    operation: VcsInspectionOperation,
    stdout: &str,
    limit: usize,
    raw_truncated: bool,
) -> Result<InspectionPayload> {
    let entries = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|path| json!({ "path": path }))
        .collect();
    finalize_payload(operation.as_str(), None, entries, limit, raw_truncated)
}

fn stat_payload(
    operation: VcsInspectionOperation,
    stdout: &str,
    limit: usize,
    raw_truncated: bool,
) -> Result<InspectionPayload> {
    let mut entries = Vec::new();
    for line in stdout.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.splitn(3, '\t');
        let added = parse_numstat_count(fields.next())?;
        let deleted = parse_numstat_count(fields.next())?;
        let path = fields
            .next()
            .filter(|path| !path.is_empty())
            .ok_or_else(|| anyhow!("numstat record is missing a path"))?;
        entries.push(json!({
            "path": path,
            "added": added,
            "deleted": deleted,
            "binary": added.is_none() || deleted.is_none()
        }));
    }
    finalize_payload(operation.as_str(), None, entries, limit, raw_truncated)
}

fn parse_numstat_count(value: Option<&str>) -> Result<Option<u64>> {
    let value = value.ok_or_else(|| anyhow!("numstat record is missing a count"))?;
    if value == "-" {
        Ok(None)
    } else {
        Ok(Some(value.parse()?))
    }
}

fn finalize_payload(
    operation: &str,
    branch: Option<String>,
    mut entries: Vec<Value>,
    limit: usize,
    raw_truncated: bool,
) -> Result<InspectionPayload> {
    let total_entries = (!raw_truncated).then_some(entries.len());
    let truncated = raw_truncated || entries.len() > limit;
    entries.truncate(limit);
    let returned_entries = entries.len();
    Ok(InspectionPayload {
        value: json!({
            "operation": operation,
            "repository": ".",
            "branch": branch,
            "entries": entries,
            "returned_entries": returned_entries,
            "total_entries": total_entries,
            "truncated": truncated
        }),
        returned_entries,
        total_entries,
        truncated,
    })
}

fn vcs_error_result(
    call_id: String,
    kind: ToolErrorKind,
    message: &'static str,
    retryable: bool,
    details: Value,
) -> ToolResult {
    let mut result = ToolResult::error(call_id, "vcs_inspect", kind, message);
    if let ToolResultStatus::Error(error) = &mut result.status {
        error.retryable = retryable;
        error.details = details;
    }
    result
}

#[cfg(test)]
#[path = "tests/vcs_inspect_tests.rs"]
mod tests;
