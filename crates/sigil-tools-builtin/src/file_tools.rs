use std::time::Duration;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Result, bail};
use async_trait::async_trait;

use serde_json::{Value, json};
use sigil_kernel::managed_file_access::{ManagedFileOutputCaptureV1, ManagedFileOutputDetailsV1};
use sigil_kernel::{
    DeclaredToolPermissionFacts, Tool, ToolAccess, ToolAnalysisStatus, ToolCategory,
    ToolConcurrencyClass, ToolContext, ToolOperation, ToolPermissionEffect,
    ToolPermissionPlanDraft, ToolPermissionSummary, ToolPreview, ToolPreviewCapability,
    ToolPreviewFile, ToolReplayContractV1, ToolResult, ToolResultMeta, ToolSemanticScope, ToolSpec,
    ToolSubjectScope, declared_tool_permission_plan, sha256_hex,
};

use crate::{
    constants::{
        DEFAULT_GLOB_LIMIT, DEFAULT_GREP_LIMIT, DEFAULT_LIST_LIMIT, DEFAULT_READ_LIMIT_LINES,
        DEFAULT_RECURSIVE_LIST_LIMIT, DEFAULT_RECURSIVE_MAX_DEPTH, DEFAULT_TEXT_LIMIT_BYTES,
        HARD_GLOB_LIMIT, HARD_GREP_LIMIT, HARD_LIST_LIMIT, HARD_READ_LIMIT_LINES,
        HARD_TEXT_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV,
    },
    support::{optional_string, optional_usize, render_unified_diff, required_string},
};

pub(crate) struct ReadFileTool;
pub(crate) struct WriteFileTool;
pub(crate) struct EditFileTool;
pub(crate) struct DeleteFileTool;

pub(crate) struct ListTool;
pub(crate) struct GlobTool;
pub(crate) struct GrepTool;

fn managed_access_receipt_value(
    outcome: &sigil_kernel::managed_file_access::ManagedFileExecutionOutcomeV1,
) -> Result<Value> {
    serde_json::to_value(&outcome.access_receipt)
        .map_err(|error| anyhow::anyhow!("failed to encode managed file access receipt: {error}"))
}

async fn execute_managed_file_operation(
    ctx: ToolContext,
    operation: sigil_kernel::managed_file_access::ManagedFileOperationV1,
    input: sigil_kernel::managed_file_access::ManagedFileExecutionInputV1,
) -> Result<sigil_kernel::managed_file_access::ManagedFileExecutionOutcomeV1> {
    use sigil_kernel::{ToolErrorKind, ToolExecutionGuardError};

    let cancellation = ctx.cancellation_handle();
    if cancellation
        .as_ref()
        .is_some_and(sigil_kernel::RunCancellationHandle::is_cancel_requested)
    {
        return Err(anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
            kind: ToolErrorKind::Interrupted,
            message: "managed file operation was cancelled before execution".to_owned(),
        }));
    }
    let task_guard = cancellation
        .as_ref()
        .map(sigil_kernel::RunCancellationHandle::register_task)
        .transpose()
        .map_err(|error| {
            anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
                kind: ToolErrorKind::Interrupted,
                message: error.to_string(),
            })
        })?;
    let execution_context = ctx
        .managed_file_execution_context(operation)
        .map_err(|error| anyhow::Error::new(ToolExecutionGuardError::tool_authority(error)))?;
    let deadline = execution_context.deadline.ok_or_else(|| {
        anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
            kind: ToolErrorKind::InvalidInput,
            message: "managed file deadline exceeds the supported range".to_owned(),
        })
    })?;
    let mut operation_task = tokio::task::spawn_blocking(move || {
        let _task_guard = task_guard;
        ctx.execute_v3_file_operation(operation, input, execution_context)
    });
    match tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        &mut operation_task,
    )
    .await
    {
        Ok(Ok(Ok(outcome))) => Ok(outcome),
        Ok(Ok(Err(error))) => Err(anyhow::Error::new(ToolExecutionGuardError::tool_authority(
            error,
        ))),
        Ok(Err(error)) => Err(anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
            kind: ToolErrorKind::Internal,
            message: format!("managed file worker failed: {error}"),
        })),
        Err(_) => {
            // Abort prevents a saturated blocking pool from starting this call later. A worker
            // that already started retains the same expired deadline and mutation ownership.
            operation_task.abort();
            let cancellation_requested = cancellation
                .as_ref()
                .is_some_and(sigil_kernel::RunCancellationHandle::is_cancel_requested);
            if cancellation_requested && let Some(cancellation) = cancellation.as_ref() {
                cancellation.mark_cleanup_incomplete();
            }
            let (kind, message) = if cancellation_requested {
                (
                    ToolErrorKind::Interrupted,
                    "managed file operation cancellation deadline exceeded; cleanup remains owned by the run"
                        .to_owned(),
                )
            } else {
                (
                    ToolErrorKind::Timeout,
                    "managed file operation exceeded its bounded execution deadline".to_owned(),
                )
            };
            let error = if matches!(
                operation,
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Write
                    | sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit
                    | sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete
                    | sigil_kernel::managed_file_access::ManagedFileOperationV1::Rename
            ) {
                ToolExecutionGuardError::EffectReconciliationRequired
            } else {
                ToolExecutionGuardError::ManagedFile { kind, message }
            };
            Err(anyhow::Error::new(error))
        }
    }
}

async fn preview_managed_file_operation(
    ctx: ToolContext,
    logical_path: String,
    operation: sigil_kernel::managed_file_access::ManagedFileOperationV1,
    max_bytes: usize,
) -> Result<sigil_kernel::managed_file_access::ManagedFilePreviewOutcomeV1> {
    use sigil_kernel::{ToolErrorKind, ToolExecutionGuardError};

    let cancellation = ctx.cancellation_handle();
    if cancellation
        .as_ref()
        .is_some_and(sigil_kernel::RunCancellationHandle::is_cancel_requested)
    {
        return Err(anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
            kind: ToolErrorKind::Interrupted,
            message: "managed file preview was cancelled before execution".to_owned(),
        }));
    }
    let task_guard = cancellation
        .as_ref()
        .map(sigil_kernel::RunCancellationHandle::register_task)
        .transpose()
        .map_err(|error| {
            anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
                kind: ToolErrorKind::Interrupted,
                message: error.to_string(),
            })
        })?;
    let timeout = Duration::from_secs(ctx.timeout_secs.max(1));
    let display_path = logical_path.clone();
    let mut preview_task = tokio::task::spawn_blocking(move || {
        let _task_guard = task_guard;
        ctx.preview_managed_file_operation(logical_path, operation, max_bytes)
    });
    match tokio::time::timeout(timeout, &mut preview_task).await {
        Ok(Ok(Ok(outcome))) => Ok(outcome),
        Ok(Ok(Err(error))) => {
            let guard = ToolExecutionGuardError::tool_authority(error);
            match guard {
                ToolExecutionGuardError::ManagedFile { kind, message } => {
                    Err(anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
                        kind,
                        message: format!("failed to read {display_path:?} for preview: {message}"),
                    }))
                }
                other => Err(anyhow::Error::new(other)),
            }
        }
        Ok(Err(error)) => Err(anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
            kind: ToolErrorKind::Internal,
            message: format!("managed file preview worker failed: {error}"),
        })),
        Err(_) => {
            preview_task.abort();
            let cancellation_requested = cancellation
                .as_ref()
                .is_some_and(sigil_kernel::RunCancellationHandle::is_cancel_requested);
            if cancellation_requested && let Some(cancellation) = cancellation.as_ref() {
                cancellation.mark_cleanup_incomplete();
            }
            let (kind, message) = if cancellation_requested {
                (
                    ToolErrorKind::Interrupted,
                    "managed file preview cancellation deadline exceeded; cleanup remains owned by the run"
                        .to_owned(),
                )
            } else {
                (
                    ToolErrorKind::Timeout,
                    "managed file preview exceeded its bounded execution deadline".to_owned(),
                )
            };
            Err(anyhow::Error::new(ToolExecutionGuardError::ManagedFile {
                kind,
                message,
            }))
        }
    }
}

async fn execute_managed_grep(
    tool: &GrepTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let pattern = required_string(&args, "pattern")?.to_owned();
    let limit = optional_usize(&args, "limit")?
        .unwrap_or(DEFAULT_GREP_LIMIT)
        .min(HARD_GREP_LIMIT);
    let outcome = execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Grep,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Grep {
            pattern,
            limit,
            max_bytes: DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES),
        },
    )
    .await?;
    let managed_access_receipt = managed_access_receipt_value(&outcome)?;
    let (binary_files_skipped, oversized_lines_skipped) = match outcome.output_details {
        ManagedFileOutputDetailsV1::Search {
            binary_files_skipped,
            oversized_lines_skipped,
        } => (binary_files_skipped, oversized_lines_skipped),
        _ => (0, 0),
    };
    let payload = outcome.payload;
    let bytes = payload.len() as u64;
    Ok(attach_streaming_artifact(
        ToolResult::ok(
            call_id,
            tool.spec().name,
            payload,
            ToolResultMeta {
                truncated: outcome.truncated,
                limit_bytes: Some(DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES) as u64),
                limit_lines: Some(limit as u64),
                returned_bytes: Some(bytes),
                returned_matches: Some(outcome.returned_lines),
                total_matches: Some(outcome.total_entries),
                details: json!({
                    "managed_access_receipt": managed_access_receipt,
                    "binary_files_skipped": binary_files_skipped,
                    "oversized_lines_skipped": oversized_lines_skipped,
                }),
                changed_files: outcome.changed_files.clone(),
                ..ToolResultMeta::default()
            },
        ),
        outcome.output_capture,
    ))
}

fn attach_streaming_artifact(
    result: ToolResult,
    capture: ManagedFileOutputCaptureV1,
) -> ToolResult {
    match capture {
        ManagedFileOutputCaptureV1::NotAttached => result,
        ManagedFileOutputCaptureV1::Published { descriptor } => {
            result.with_captured_artifact(*descriptor)
        }
        ManagedFileOutputCaptureV1::Unavailable { observed_bytes } => {
            result.with_unavailable_artifact_capture(observed_bytes)
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read one UTF-8 text file from the workspace. Pass a workspace-relative file path such as src/lib.rs; this tool does not list directories."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "offset": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn concurrency_class(&self) -> ToolConcurrencyClass {
        ToolConcurrencyClass::ParallelReadOnly
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::pure_read()
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let path = required_string(args, "path")?;
        let offset = optional_usize(args, "offset")?.unwrap_or(0);
        let limit = optional_usize(args, "limit")?
            .unwrap_or(DEFAULT_READ_LIMIT_LINES)
            .min(HARD_READ_LIMIT_LINES);
        let operation_scope =
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Read {
                offset,
                limit,
                max_bytes: DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES),
            }
            .operation_scope();
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Read,
                network_effect: None,
                subjects: vec![file_permission_subject(&ctx.workspace_root, path)?],
                tool_default_mode: None,
                managed_file_access: Some(file_access_ref(
                    ctx,
                    path,
                    &operation_scope,
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_read(self, ctx, call_id, args).await
    }
}

async fn execute_managed_read(
    tool: &ReadFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let offset = optional_usize(&args, "offset")?.unwrap_or(0);
    let limit = optional_usize(&args, "limit")?
        .unwrap_or(DEFAULT_READ_LIMIT_LINES)
        .min(HARD_READ_LIMIT_LINES);
    let outcome = match execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Read {
            offset,
            limit,
            max_bytes: DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES),
        },
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(error) => {
            use sigil_kernel::{ToolErrorKind, ToolExecutionGuardError};
            match error.downcast_ref::<ToolExecutionGuardError>() {
                Some(ToolExecutionGuardError::ManagedFile {
                    kind: ToolErrorKind::NotFound,
                    ..
                }) => {
                    let file_name = Path::new(&path)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or("*");
                    let pattern = format!("**/{file_name}");
                    return Ok(ToolResult::error(call_id, tool.spec().name, ToolErrorKind::NotFound,
                        format!("read_file path {path:?} does not exist; discover the exact workspace-relative path with glob pattern {pattern:?}; do not guess another path"))
                        .with_error_details(true, json!({"requested_path": path, "recovery": "discover_path", "suggested_tool": "glob", "suggested_pattern": pattern})));
                }
                Some(ToolExecutionGuardError::ManagedFile {
                    kind: ToolErrorKind::InvalidInput,
                    message,
                }) => {
                    return Ok(ToolResult::error(
                        call_id,
                        tool.spec().name,
                        ToolErrorKind::InvalidInput,
                        format!(
                            "read_file path {path:?} cannot be read as a regular UTF-8 file: {message}; use a workspace-relative file path such as src/lib.rs"
                        ),
                    ));
                }
                _ => return Err(error),
            }
        }
    };
    let mut details = serde_json::Map::new();
    details.insert("path".to_owned(), json!(path));
    details.insert("offset".to_owned(), json!(offset));
    if let Some(language) = read_file_language(&path) {
        details.insert("language".to_owned(), json!(language));
    }
    details.insert(
        "managed_access_receipt".to_owned(),
        managed_access_receipt_value(&outcome)?,
    );
    let selected_bytes = match outcome.output_details {
        ManagedFileOutputDetailsV1::Read {
            selected_bytes,
            next_offset,
            oversized_lines,
        } => {
            if let Some(next_offset) = next_offset {
                details.insert("next_offset".to_owned(), json!(next_offset));
            }
            if oversized_lines > 0 {
                details.insert(
                    "policy_omitted_oversized_lines".to_owned(),
                    json!(oversized_lines),
                );
            }
            selected_bytes
        }
        _ => outcome.observed_bytes,
    };
    let mut payload = outcome.payload;
    let returned_bytes = payload.len() as u64;
    if outcome.truncated {
        crate::support::append_truncation_notice(&mut payload);
    }
    Ok(attach_streaming_artifact(
        ToolResult::ok(
            call_id,
            tool.spec().name,
            payload.clone(),
            ToolResultMeta {
                bytes: Some(outcome.observed_bytes),
                truncated: outcome.truncated,
                limit_bytes: Some(DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES) as u64),
                limit_lines: Some(limit as u64),
                returned_bytes: Some(returned_bytes),
                omitted_bytes: Some(selected_bytes.saturating_sub(returned_bytes)),
                returned_lines: Some(outcome.returned_lines),
                total_bytes: Some(selected_bytes),
                total_lines: Some(outcome.total_lines),
                details: Value::Object(details),
                ..ToolResultMeta::default()
            },
        ),
        outcome.output_capture,
    ))
}

/// Every file tool obtains its subject identity and operation digest from the authority.
fn file_access_ref(
    ctx: &ToolContext,
    subject: &str,
    scope: &str,
    operation: sigil_kernel::managed_file_access::ManagedFileOperationV1,
) -> Result<sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1> {
    ctx.plan_managed_file_access(subject.to_owned(), operation, scope.to_owned())
        .map_err(|error| anyhow::anyhow!("managed file planning refused: {error}"))
}

fn read_file_language(path: &str) -> Option<&'static str> {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .or_else(|| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| name.eq_ignore_ascii_case("Dockerfile"))
                .map(|_| "dockerfile".to_owned())
        })?;
    match extension.as_str() {
        "rs" => Some("rust"),
        "toml" | "lock" => Some("toml"),
        "json" | "jsonl" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "js" | "jsx" => Some("javascript"),
        "ts" | "tsx" => Some("typescript"),
        "py" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "c" | "h" => Some("c"),
        "cc" | "cpp" | "cxx" | "hpp" => Some("cpp"),
        "cs" => Some("c#"),
        "swift" => Some("swift"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "sh" | "bash" | "zsh" | "fish" => Some("bash"),
        "sql" => Some("sql"),
        "html" => Some("html"),
        "css" | "scss" | "sass" => Some("css"),
        "xml" | "svg" => Some("xml"),
        "lua" => Some("lua"),
        "vim" => Some("vim"),
        "dockerfile" => Some("dockerfile"),
        _ => None,
    }
}

fn file_permission_subject(
    _workspace_root: &Path,
    path: &str,
) -> Result<sigil_kernel::ToolSubject> {
    sigil_kernel::managed_file_access::ManagedFileLogicalPathV1::new(path.to_owned())
        .map_err(|error| anyhow::anyhow!("invalid managed file path: {error}"))?;
    Ok(sigil_kernel::ToolSubject::path_with_scope(
        path,
        path,
        None,
        ToolSubjectScope::Workspace,
    ))
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: format!(
                "Write UTF-8 content to a workspace-relative file. For temporary shell files, use ${SIGIL_SCRATCH_DIR_ENV} with bash or terminal_start (shown as cache/tmp).",
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::reconciliable("prepared_workspace_mutation_v1")
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let content = required_string(args, "content")?;
        let operation_scope =
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Write {
                content: content.to_owned(),
            }
            .operation_scope();
        let path = required_string(args, "path")?;
        let file_access = file_access_ref(
            ctx,
            path,
            &operation_scope,
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
        )?;
        let operation = if file_access.target_exists {
            ToolOperation::OverwriteFile
        } else {
            ToolOperation::CreateFile
        };
        let mut effects = BTreeSet::from([ToolPermissionEffect::FileWrite]);
        if operation == ToolOperation::OverwriteFile {
            effects.insert(ToolPermissionEffect::FileRead);
        }
        let mut semantic_scope = ToolSemanticScope::new("workspace:file_write", 1);
        semantic_scope
            .qualifiers
            .insert("operation".to_owned(), operation.as_str().to_owned());
        semantic_scope
            .qualifiers
            .insert("content_sha256".to_owned(), sha256_hex(content.as_bytes()));
        Ok(ToolPermissionPlanDraft {
            access: ToolAccess::Write,
            operation,
            effects,
            subjects: vec![file_permission_subject(&ctx.workspace_root, path)?],
            analysis: ToolAnalysisStatus::Complete,
            containment: Default::default(),
            semantic_scope: Some(semantic_scope),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([(
                "planner".to_owned(),
                "typed_file_write_v2".to_owned(),
            )]),
            safe_summary: ToolPermissionSummary {
                title: if operation == ToolOperation::CreateFile {
                    "Create workspace file".to_owned()
                } else {
                    "Overwrite workspace file".to_owned()
                },
                detail: "Write one approval-bound workspace file".to_owned(),
                step_count: 1,
                workspace_code_steps: 0,
            },
            managed_file_access: Some(file_access),
        })
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_write(self, ctx, call_id, args).await
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        {
            let path = required_string(&args, "path")?.to_owned();
            let content = required_string(&args, "content")?.to_owned();
            let current = preview_managed_file_operation(
                ctx,
                path.clone(),
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
                HARD_TEXT_LIMIT_BYTES,
            )
            .await?
            .payload;
            let diff = render_unified_diff(
                &current,
                &content,
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            Ok(Some(ToolPreview {
                title: format!("Update {path}"),
                summary: format!("Update {} lines in {path}", content.lines().count().max(1)),
                body: diff.clone(),
                changed_files: vec![path.to_owned()],
                file_diffs: vec![ToolPreviewFile {
                    path: path.to_owned(),
                    diff,
                }],
            }))
        }
    }
}

async fn execute_managed_write(
    tool: &WriteFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let content = required_string(&args, "content")?.to_owned();
    let outcome = execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Write { content },
    )
    .await?;
    let managed_access_receipt = managed_access_receipt_value(&outcome)?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        format!("wrote {path}"),
        ToolResultMeta {
            bytes: Some(outcome.observed_bytes),
            details: json!({
                "path": path,
                "managed_access_receipt": managed_access_receipt,
            }),
            changed_files: outcome.changed_files.clone(),
            ..ToolResultMeta::default()
        },
    ))
}

#[async_trait]
impl Tool for EditFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".to_owned(),
            description: "Replace an exact text snippet in a workspace file.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "old_text": { "type": "string" },
                    "new_text": { "type": "string" }
                },
                "required": ["path", "old_text", "new_text"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::reconciliable("prepared_workspace_mutation_v1")
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let old_text = required_string(args, "old_text")?;
        let new_text = required_string(args, "new_text")?;
        let path = required_string(args, "path")?;
        let operation_scope =
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Edit {
                old_text: old_text.to_owned(),
                new_text: new_text.to_owned(),
            }
            .operation_scope();
        let mut semantic_scope = ToolSemanticScope::new("workspace:file_edit", 1);
        semantic_scope.qualifiers.insert(
            "replacement_sha256".to_owned(),
            sha256_hex(format!("{old_text}\0{new_text}").as_bytes()),
        );
        Ok(ToolPermissionPlanDraft {
            access: ToolAccess::Write,
            operation: ToolOperation::EditFile,
            effects: BTreeSet::from([
                ToolPermissionEffect::FileRead,
                ToolPermissionEffect::FileWrite,
            ]),
            subjects: vec![file_permission_subject(&ctx.workspace_root, path)?],
            analysis: ToolAnalysisStatus::Complete,
            containment: Default::default(),
            semantic_scope: Some(semantic_scope),
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([(
                "planner".to_owned(),
                "typed_file_edit_v2".to_owned(),
            )]),
            safe_summary: ToolPermissionSummary {
                title: "Edit workspace file".to_owned(),
                detail: "Read and replace one exact snippet in a workspace file".to_owned(),
                step_count: 1,
                workspace_code_steps: 0,
            },
            managed_file_access: Some(file_access_ref(
                ctx,
                path,
                &operation_scope,
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
            )?),
        })
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_edit(self, ctx, call_id, args).await
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        {
            let path = required_string(&args, "path")?.to_owned();
            let old_text = required_string(&args, "old_text")?.to_owned();
            let new_text = required_string(&args, "new_text")?.to_owned();
            let old_len = old_text.chars().count();
            let new_len = new_text.chars().count();
            let original = preview_managed_file_operation(
                ctx,
                path.clone(),
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
                HARD_TEXT_LIMIT_BYTES,
            )
            .await?
            .payload;
            let occurrences = original.matches(&old_text).count();
            if occurrences == 0 {
                bail!("old_text not found in {path}");
            }
            if occurrences > 1 {
                bail!("old_text is ambiguous in {path}");
            }
            let updated = original.replacen(&old_text, &new_text, 1);
            let diff = render_unified_diff(
                &original,
                &updated,
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            Ok(Some(ToolPreview {
                title: format!("Edit {path}"),
                summary: format!("Replace {old_len} chars with {new_len} chars in {path}"),
                body: diff.clone(),
                changed_files: vec![path.to_owned()],
                file_diffs: vec![ToolPreviewFile {
                    path: path.to_owned(),
                    diff,
                }],
            }))
        }
    }
}

async fn execute_managed_edit(
    tool: &EditFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let old_text = required_string(&args, "old_text")?.to_owned();
    let new_text = required_string(&args, "new_text")?.to_owned();
    let outcome = execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Edit { old_text, new_text },
    )
    .await?;
    let managed_access_receipt = managed_access_receipt_value(&outcome)?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        format!("edited {path}"),
        ToolResultMeta {
            bytes: Some(outcome.observed_bytes),
            details: json!({
                "path": path,
                "managed_access_receipt": managed_access_receipt,
            }),
            changed_files: outcome.changed_files.clone(),
            ..ToolResultMeta::default()
        },
    ))
}

#[async_trait]
impl Tool for DeleteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "delete_file".to_owned(),
            description: "Delete a regular workspace file after approval.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::reconciliable("prepared_workspace_mutation_v1")
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let path = required_string(args, "path")?;
        Ok(ToolPermissionPlanDraft {
            access: ToolAccess::Write,
            operation: ToolOperation::DeleteFile,
            effects: BTreeSet::from([
                ToolPermissionEffect::FileRead,
                ToolPermissionEffect::FileDelete,
            ]),
            subjects: vec![file_permission_subject(&ctx.workspace_root, path)?],
            analysis: ToolAnalysisStatus::Complete,
            containment: Default::default(),
            semantic_scope: None,
            tool_default_mode: None,
            analysis_bindings: BTreeMap::from([(
                "planner".to_owned(),
                "typed_file_delete_v2".to_owned(),
            )]),
            safe_summary: ToolPermissionSummary {
                title: "Delete workspace file".to_owned(),
                detail: "Inspect and delete one approval-bound workspace file".to_owned(),
                step_count: 1,
                workspace_code_steps: 0,
            },
            managed_file_access: Some(file_access_ref(
                ctx,
                path,
                &sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Delete
                    .operation_scope(),
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
            )?),
        })
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_delete(self, ctx, call_id, args).await
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        {
            let path = required_string(&args, "path")?.to_owned();
            let current = preview_managed_file_operation(
                ctx,
                path.clone(),
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
                HARD_TEXT_LIMIT_BYTES,
            )
            .await?
            .payload;
            let diff = render_unified_diff(
                &current,
                "",
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            Ok(Some(ToolPreview {
                title: format!("Delete {path}"),
                summary: format!(
                    "Delete {} lines from {path}",
                    current.lines().count().max(1)
                ),
                body: diff.clone(),
                changed_files: vec![path.clone()],
                file_diffs: vec![ToolPreviewFile { path, diff }],
            }))
        }
    }
}

async fn execute_managed_delete(
    tool: &DeleteFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let outcome = execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Delete,
    )
    .await
    .map_err(|error| {
        use sigil_kernel::{ToolErrorKind, ToolExecutionGuardError};
        let message = match error.downcast_ref::<ToolExecutionGuardError>() {
            Some(ToolExecutionGuardError::ManagedFile {
                kind: ToolErrorKind::NotFound,
                ..
            }) => Some((
                ToolErrorKind::NotFound,
                format!("delete_file failed to inspect {path:?}: file does not exist"),
            )),
            Some(ToolExecutionGuardError::ManagedFile {
                kind: ToolErrorKind::InvalidInput,
                ..
            }) => Some((
                ToolErrorKind::InvalidInput,
                format!("delete_file only supports regular files: {path}"),
            )),
            _ => None,
        };
        message.map_or(error, |(kind, message)| {
            anyhow::Error::new(ToolExecutionGuardError::ManagedFile { kind, message })
        })
    })?;
    let managed_access_receipt = managed_access_receipt_value(&outcome)?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        format!("deleted {path}"),
        ToolResultMeta {
            bytes: Some(outcome.observed_bytes),
            details: json!({
                "path": path,
                "action": "delete",
                "managed_access_receipt": managed_access_receipt,
            }),
            changed_files: outcome.changed_files.clone(),
            ..ToolResultMeta::default()
        },
    ))
}

#[async_trait]
impl Tool for ListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ls".to_owned(),
            description: "List files and directories inside the workspace.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "recursive": { "type": "boolean" },
                    "limit": { "type": "integer" },
                    "max_depth": { "type": "integer" }
                }
            }),
            category: ToolCategory::File,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn concurrency_class(&self) -> ToolConcurrencyClass {
        ToolConcurrencyClass::ParallelReadOnly
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::pure_read()
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let path = optional_string(args, "path").unwrap_or(".");
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let limit = optional_usize(args, "limit")?
            .unwrap_or(if recursive {
                DEFAULT_RECURSIVE_LIST_LIMIT
            } else {
                DEFAULT_LIST_LIMIT
            })
            .min(HARD_LIST_LIMIT);
        let max_depth = optional_usize(args, "max_depth")?.unwrap_or(DEFAULT_RECURSIVE_MAX_DEPTH);
        let operation_scope =
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::List {
                recursive,
                limit,
                max_depth,
            }
            .operation_scope();
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Search,
                network_effect: None,
                subjects: vec![file_permission_subject(&ctx.workspace_root, path)?],
                tool_default_mode: None,
                managed_file_access: Some(file_access_ref(
                    ctx,
                    path,
                    &operation_scope,
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::List,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_list(self, ctx, call_id, args).await
    }
}

async fn execute_managed_list(
    tool: &ListTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let recursive = args
        .get("recursive")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let limit = optional_usize(&args, "limit")?
        .unwrap_or(if recursive {
            DEFAULT_RECURSIVE_LIST_LIMIT
        } else {
            DEFAULT_LIST_LIMIT
        })
        .min(HARD_LIST_LIMIT);
    let max_depth = optional_usize(&args, "max_depth")?.unwrap_or(DEFAULT_RECURSIVE_MAX_DEPTH);
    let outcome = execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::List,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::List {
            recursive,
            limit,
            max_depth,
        },
    )
    .await?;
    let managed_access_receipt = managed_access_receipt_value(&outcome)?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            truncated: outcome.truncated,
            limit_lines: Some(limit as u64),
            returned_entries: Some(outcome.returned_entries),
            total_entries: Some(outcome.total_entries),
            details: json!({
                "managed_access_receipt": managed_access_receipt,
            }),
            changed_files: outcome.changed_files.clone(),
            ..ToolResultMeta::default()
        },
    ))
}

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".to_owned(),
            description: "Return workspace files matching a glob pattern.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
            category: ToolCategory::Search,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::pure_read()
    }

    fn concurrency_class(&self) -> ToolConcurrencyClass {
        ToolConcurrencyClass::ParallelReadOnly
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let pattern = required_string(args, "pattern")?;
        let limit = optional_usize(args, "limit")?
            .unwrap_or(DEFAULT_GLOB_LIMIT)
            .min(HARD_GLOB_LIMIT);
        let operation_scope =
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Glob {
                pattern: pattern.to_owned(),
                limit,
            }
            .operation_scope();
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Search,
                network_effect: None,
                subjects: vec![file_permission_subject(&ctx.workspace_root, ".")?],
                tool_default_mode: None,
                managed_file_access: Some(file_access_ref(
                    ctx,
                    ".",
                    &operation_scope,
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Glob,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_glob(self, ctx, call_id, args).await
    }
}

async fn execute_managed_glob(
    tool: &GlobTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let pattern = required_string(&args, "pattern")?.to_owned();
    let limit = optional_usize(&args, "limit")?
        .unwrap_or(DEFAULT_GLOB_LIMIT)
        .min(HARD_GLOB_LIMIT);
    let outcome = execute_managed_file_operation(
        ctx,
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Glob,
        sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Glob { pattern, limit },
    )
    .await?;
    let managed_access_receipt = managed_access_receipt_value(&outcome)?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            truncated: outcome.truncated,
            limit_lines: Some(limit as u64),
            returned_entries: Some(outcome.returned_entries),
            total_entries: Some(outcome.total_entries),
            details: json!({
                "managed_access_receipt": managed_access_receipt,
                "returned_paths": outcome.returned_entries,
                "total_paths": outcome.total_entries,
            }),
            changed_files: outcome.changed_files.clone(),
            ..ToolResultMeta::default()
        },
    ))
}

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".to_owned(),
            description: "Search workspace files with a regex pattern.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string" },
                    "path": { "type": "string" },
                    "limit": { "type": "integer" }
                },
                "required": ["pattern"]
            }),
            category: ToolCategory::Search,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn concurrency_class(&self) -> ToolConcurrencyClass {
        ToolConcurrencyClass::ParallelReadOnly
    }

    fn replay_contract(&self) -> ToolReplayContractV1 {
        ToolReplayContractV1::pure_read()
    }

    fn permission_plan(&self, ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let pattern = required_string(args, "pattern")?;
        let path = optional_string(args, "path").unwrap_or(".");
        let limit = optional_usize(args, "limit")?
            .unwrap_or(DEFAULT_GREP_LIMIT)
            .min(HARD_GREP_LIMIT);
        let operation_scope =
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Grep {
                pattern: pattern.to_owned(),
                limit,
                max_bytes: DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES),
            }
            .operation_scope();
        let spec = self.spec();
        declared_tool_permission_plan(
            &spec,
            args,
            DeclaredToolPermissionFacts {
                access: ToolAccess::Read,
                operation: ToolOperation::Search,
                network_effect: None,
                subjects: vec![file_permission_subject(&ctx.workspace_root, path)?],
                tool_default_mode: None,
                managed_file_access: Some(file_access_ref(
                    ctx,
                    path,
                    &operation_scope,
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Grep,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        execute_managed_grep(self, ctx, call_id, args).await
    }
}
