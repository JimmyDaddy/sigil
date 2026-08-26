#[cfg(test)]
use std::fs::{self, File};
#[cfg(test)]
use std::io::{BufRead, BufReader, Write};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[cfg(test)]
use anyhow::Context;
use anyhow::{Result, bail};
use async_trait::async_trait;
#[cfg(test)]
use globset::{Glob, GlobSetBuilder};
#[cfg(test)]
use ignore::WalkBuilder;
#[cfg(test)]
use regex::Regex;
use serde_json::{Value, json};
use sigil_kernel::{
    DeclaredToolPermissionFacts, Tool, ToolAccess, ToolAnalysisStatus, ToolCategory,
    ToolConcurrencyClass, ToolContext, ToolOperation, ToolPermissionEffect,
    ToolPermissionPlanDraft, ToolPermissionSummary, ToolPreview, ToolPreviewCapability,
    ToolPreviewFile, ToolReplayContractV1, ToolResult, ToolResultMeta, ToolSemanticScope, ToolSpec,
    ToolSubjectScope, declared_tool_permission_plan, sha256_hex,
};
#[cfg(test)]
use sigil_kernel::{
    ToolArtifactDescriptorV1, ToolArtifactEncoding, ToolArtifactSensitivity, ToolErrorKind,
    safe_persistence_json_value, safe_persistence_text,
};

#[cfg(test)]
use crate::path::lexically_normalize_path;
#[cfg(test)]
use crate::path::relativize;
#[cfg(test)]
use crate::path::resolve_tool_path;
#[cfg(test)]
use crate::path::{
    canonical_workspace_root, resolve_delete_file_target, resolve_workspace_path,
    validate_delete_file_target,
};
#[cfg(test)]
use crate::support::run_blocking_io;
#[cfg(test)]
use crate::support::{append_truncation_notice, truncate_line_for_model};
use crate::{
    constants::{
        DEFAULT_GLOB_LIMIT, DEFAULT_GREP_LIMIT, DEFAULT_LIST_LIMIT, DEFAULT_READ_LIMIT_LINES,
        DEFAULT_RECURSIVE_LIST_LIMIT, DEFAULT_RECURSIVE_MAX_DEPTH, DEFAULT_TEXT_LIMIT_BYTES,
        HARD_GLOB_LIMIT, HARD_GREP_LIMIT, HARD_LIST_LIMIT, HARD_READ_LIMIT_LINES,
        HARD_TEXT_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV,
    },
    support::{optional_string, optional_usize, render_unified_diff, required_string},
};
#[cfg(test)]
use sigil_kernel::{delete_file_with_mutation, write_file_with_mutation};

pub(crate) struct ReadFileTool;
pub(crate) struct WriteFileTool;
pub(crate) struct EditFileTool;
pub(crate) struct DeleteFileTool;

pub(crate) struct ListTool;
pub(crate) struct GlobTool;
pub(crate) struct GrepTool;

#[cfg(test)]
enum ReadFileLoad {
    File {
        content: String,
        bytes: u64,
        returned_bytes: u64,
        returned_lines: u64,
        selected_bytes: u64,
        total_lines: u64,
        truncated: bool,
        next_offset: Option<usize>,
        artifact: StreamingArtifactCapture,
        oversized_lines: u64,
    },
    Missing,
    NotAFile,
}

#[cfg(not(test))]
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
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Grep,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Grep {
                pattern,
                limit,
                max_bytes: DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES),
            },
        )
        .map_err(|error| anyhow::anyhow!("managed file access refused: {error}"))?;
    let payload = outcome.payload;
    let bytes = payload.len() as u64;
    Ok(ToolResult::ok(
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
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
enum StreamingArtifactCapture {
    NotAttached,
    Published(Box<ToolArtifactDescriptorV1>),
    Unavailable { observed_bytes: u64 },
}

#[cfg(test)]
const MAX_STREAMED_TEXT_LINE_BYTES: usize = 1024 * 1024;

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
                    "read-file",
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_read(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_read_file(self, ctx, call_id, args).await
        }
    }
}

#[cfg(not(test))]
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
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Read {
                offset,
                limit,
                max_bytes: DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES),
            },
        )
        .map_err(|error| anyhow::anyhow!("managed file access refused: {error}"))?;
    let mut details = serde_json::Map::new();
    details.insert("path".to_owned(), json!(path));
    details.insert("offset".to_owned(), json!(offset));
    if let Some(language) = read_file_language(&path) {
        details.insert("language".to_owned(), json!(language));
    }
    let payload = outcome.payload;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        payload.clone(),
        ToolResultMeta {
            bytes: Some(outcome.observed_bytes),
            truncated: outcome.truncated,
            limit_bytes: Some(DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES) as u64),
            limit_lines: Some(limit as u64),
            returned_bytes: Some(payload.len() as u64),
            returned_lines: Some(outcome.returned_lines),
            total_bytes: Some(outcome.observed_bytes),
            total_lines: Some(outcome.total_lines),
            details: Value::Object(details),
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
struct BoundedLogicalLine {
    bytes: Vec<u8>,
    observed_body_bytes: u64,
    oversized: bool,
}

#[cfg(test)]
fn read_bounded_logical_line(
    reader: &mut impl BufRead,
) -> std::io::Result<Option<BoundedLogicalLine>> {
    let mut bytes = Vec::new();
    let mut observed_bytes = 0u64;
    let mut oversized = false;
    let mut saw_data = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        saw_data = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        let body_len = newline.unwrap_or(available.len());
        observed_bytes = observed_bytes.saturating_add(body_len as u64);
        if !oversized {
            let remaining = MAX_STREAMED_TEXT_LINE_BYTES.saturating_sub(bytes.len());
            if body_len <= remaining {
                bytes.extend_from_slice(&available[..body_len]);
            } else {
                bytes.clear();
                oversized = true;
            }
        }
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }
    if !saw_data {
        return Ok(None);
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
        observed_bytes = observed_bytes.saturating_sub(1);
    }
    Ok(Some(BoundedLogicalLine {
        bytes,
        observed_body_bytes: observed_bytes,
        oversized,
    }))
}

#[cfg(test)]
fn project_policy_safe_line(
    line: BoundedLogicalLine,
    line_number: usize,
    path: &Path,
) -> Result<(String, bool)> {
    if line.oversized {
        return Ok((
            format!(
                "[sigil: policy omitted oversized line {line_number} ({} bytes)]",
                line.observed_body_bytes
            ),
            true,
        ));
    }
    let raw = std::str::from_utf8(&line.bytes)
        .with_context(|| format!("failed to decode UTF-8 text from {}", path.display()))?;
    let safe = safe_persistence_text(raw);
    let redacted = safe != raw;
    Ok((safe, redacted))
}

#[cfg(test)]
fn attach_streaming_artifact(result: ToolResult, artifact: StreamingArtifactCapture) -> ToolResult {
    match artifact {
        StreamingArtifactCapture::NotAttached => result,
        StreamingArtifactCapture::Published(descriptor) => {
            result.with_captured_artifact(*descriptor)
        }
        StreamingArtifactCapture::Unavailable { observed_bytes } => {
            result.with_unavailable_artifact_capture(observed_bytes)
        }
    }
}

/// RFC-0071 R71.9b: one managed file-access planner for every in-process file tool.
///
/// Shipping planning delegates subject identity, generation, resolver proof and plan hashing to
/// the authority. The compatibility fallback is test-only and is never compiled into a normal
/// shipping build.
fn file_access_ref(
    ctx: &ToolContext,
    subject: &str,
    scope: &str,
    operation: sigil_kernel::managed_file_access::ManagedFileOperationV1,
) -> Result<sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1> {
    if ctx.tool_authority().is_some() {
        return ctx
            .plan_managed_file_access(subject.to_owned(), operation, scope.to_owned())
            .map_err(|error| anyhow::anyhow!("managed file planning refused: {error}"));
    }
    #[cfg(not(test))]
    {
        Err(anyhow::anyhow!(
            "managed file planning requires an active authority composition"
        ))
    }
    #[cfg(test)]
    let workspace_root = &ctx.workspace_root;
    #[cfg(test)]
    use sha2::Digest;
    #[cfg(test)]
    use sigil_kernel::resource::CanonicalHash;
    #[cfg(test)]
    let resolved = resolve_workspace_path(workspace_root, subject)?;
    #[cfg(test)]
    let normalized = lexically_normalize_path(&resolved)?
        .to_string_lossy()
        .into_owned();
    #[cfg(test)]
    let mut hasher = sha2::Sha256::new();
    #[cfg(test)]
    hasher.update(normalized.as_bytes());
    #[cfg(test)]
    let subject_binding_hash = CanonicalHash::from_bytes(hasher.finalize().into());
    #[cfg(test)]
    let mut op = sha2::Sha256::new();
    #[cfg(test)]
    op.update(scope.as_bytes());
    #[cfg(test)]
    op.update(b"::");
    #[cfg(test)]
    op.update(normalized.as_bytes());
    #[cfg(test)]
    let operation_digest = CanonicalHash::from_bytes(op.finalize().into());
    #[cfg(test)]
    {
        Ok(
            sigil_kernel::permission_plan_v3::ManagedFileAccessPlanDraftRefV1 {
                plan_id: sigil_kernel::resource::OpaqueManagedFileAccessPlanId::new(format!(
                    "{scope}:{}",
                    normalized.trim_start_matches('/').replace(['/', '.'], "-")
                )),
                subject_ref: sigil_kernel::resource::OpaquePermissionSubjectRef::new(
                    normalized.clone(),
                ),
                subject_binding_hash,
                operation_digest,
                authority_generation: sigil_kernel::resource::AuthorityGeneration {
                    epoch: 0,
                    instance_hash: CanonicalHash::from_bytes([0u8; 32]),
                },
                resolver_proof_digest: CanonicalHash::from_bytes([0u8; 32]),
                plan_hash: CanonicalHash::from_bytes([0u8; 32]),
            },
        )
    }
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

#[cfg(not(test))]
fn write_file_permission_operation(_ctx: &ToolContext, _args: &Value) -> Result<ToolOperation> {
    // Existence is an authority-owned fact. Permission planning must not inspect the workspace
    // and therefore cannot distinguish create from overwrite before the managed executor runs.
    Ok(ToolOperation::OverwriteFile)
}

#[cfg(test)]
fn write_file_permission_operation(ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
    let path = required_string(args, "path")?;
    let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
    let requested_path = Path::new(path);
    let target = if requested_path.is_absolute() {
        lexically_normalize_path(requested_path)?
    } else {
        lexically_normalize_path(&workspace_root.join(requested_path))?
    };
    let resolved =
        crate::path::resolve_tool_path_from_base(&workspace_root, &workspace_root, path)?;
    if resolved.scope != ToolSubjectScope::Workspace {
        bail!("write_file path is outside workspace: {path}");
    }
    if target.exists() {
        Ok(ToolOperation::OverwriteFile)
    } else {
        Ok(ToolOperation::CreateFile)
    }
}

#[cfg(not(test))]
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

#[cfg(test)]
fn file_permission_subject(workspace_root: &Path, path: &str) -> Result<sigil_kernel::ToolSubject> {
    crate::path::tool_path_subject(workspace_root, path)
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: format!(
                "Write UTF-8 content to a workspace file. For temporary shell files, use ${SIGIL_SCRATCH_DIR_ENV} with bash or terminal_start (shown as cache/tmp); OS temp directories are outside the workspace and require permission.external_directory.",
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
        let path = required_string(args, "path")?;
        let operation = write_file_permission_operation(ctx, args)?;
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
            managed_file_access: Some(file_access_ref(
                ctx,
                path,
                "write-file",
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
            )?),
        })
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_write(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_write_file(self, ctx, call_id, args).await
        }
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        #[cfg(not(test))]
        {
            let path = required_string(&args, "path")?.to_owned();
            let content = required_string(&args, "content")?.to_owned();
            let current = ctx
                .preview_managed_file_operation(
                    path.clone(),
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
                    HARD_TEXT_LIMIT_BYTES,
                )
                .map_err(|error| anyhow::anyhow!("managed file preview refused: {error}"))?
                .payload;
            let diff = render_unified_diff(
                &current,
                &content,
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            return Ok(Some(ToolPreview {
                title: format!("Update {path}"),
                summary: format!("Update {} lines in {path}", content.lines().count().max(1)),
                body: diff.clone(),
                changed_files: vec![path.to_owned()],
                file_diffs: vec![ToolPreviewFile {
                    path: path.to_owned(),
                    diff,
                }],
            }));
        }
        #[cfg(test)]
        {
            let path = required_string(&args, "path")?.to_owned();
            let content = required_string(&args, "content")?.to_owned();
            let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
            let (current, action) = run_blocking_io("write_file_preview", move || {
                if resolved.exists() {
                    let current = fs::read_to_string(&resolved)
                        .with_context(|| format!("failed to read {}", resolved.display()))?;
                    Ok((current, "Update"))
                } else {
                    Ok((String::new(), "Create"))
                }
            })
            .await?;
            let diff = render_unified_diff(
                &current,
                &content,
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            Ok(Some(ToolPreview {
                title: format!("{action} {path}"),
                summary: format!(
                    "{action} {} lines in {path}",
                    content.lines().count().max(1)
                ),
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

#[cfg(not(test))]
async fn execute_managed_write(
    tool: &WriteFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let content = required_string(&args, "content")?.to_owned();
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Write { content },
        )
        .map_err(|error| anyhow::anyhow!("managed file write refused: {error}"))?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            bytes: Some(outcome.observed_bytes),
            details: json!({"path": path}),
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
                "edit-file",
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
            )?),
        })
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_edit(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_edit_file(self, ctx, call_id, args).await
        }
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        #[cfg(not(test))]
        {
            let path = required_string(&args, "path")?.to_owned();
            let old_text = required_string(&args, "old_text")?.to_owned();
            let new_text = required_string(&args, "new_text")?.to_owned();
            let old_len = old_text.chars().count();
            let new_len = new_text.chars().count();
            let original = ctx
                .preview_managed_file_operation(
                    path.clone(),
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
                    HARD_TEXT_LIMIT_BYTES,
                )
                .map_err(|error| anyhow::anyhow!("managed file preview refused: {error}"))?
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
            return Ok(Some(ToolPreview {
                title: format!("Edit {path}"),
                summary: format!("Replace {old_len} chars with {new_len} chars in {path}"),
                body: diff.clone(),
                changed_files: vec![path.to_owned()],
                file_diffs: vec![ToolPreviewFile {
                    path: path.to_owned(),
                    diff,
                }],
            }));
        }
        #[cfg(test)]
        {
            let path = required_string(&args, "path")?.to_owned();
            let old_text = required_string(&args, "old_text")?.to_owned();
            let new_text = required_string(&args, "new_text")?.to_owned();
            let old_len = old_text.chars().count();
            let new_len = new_text.chars().count();
            let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
            let error_path = path.clone();
            let (original, updated) = run_blocking_io("edit_file_preview", move || {
                let original = fs::read_to_string(&resolved)
                    .with_context(|| format!("failed to read {}", resolved.display()))?;
                let occurrences = original.matches(&old_text).count();
                if occurrences == 0 {
                    bail!("old_text not found in {}", error_path);
                }
                if occurrences > 1 {
                    bail!("old_text is ambiguous in {}", error_path);
                }
                let updated = original.replacen(&old_text, &new_text, 1);
                Ok((original, updated))
            })
            .await?;
            let diff = render_unified_diff(
                &original,
                &updated,
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            Ok(Some(ToolPreview {
                title: format!("Edit {path}"),
                summary: format!("Replace {} chars with {} chars in {path}", old_len, new_len,),
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

#[cfg(not(test))]
async fn execute_managed_edit(
    tool: &EditFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let old_text = required_string(&args, "old_text")?.to_owned();
    let new_text = required_string(&args, "new_text")?.to_owned();
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Edit {
                old_text,
                new_text,
            },
        )
        .map_err(|error| anyhow::anyhow!("managed file edit refused: {error}"))?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            bytes: Some(outcome.observed_bytes),
            details: json!({"path": path}),
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
                "delete-file",
                sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
            )?),
        })
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_delete(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_delete_file(self, ctx, call_id, args).await
        }
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        #[cfg(not(test))]
        {
            let path = required_string(&args, "path")?.to_owned();
            let current = ctx
                .preview_managed_file_operation(
                    path.clone(),
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
                    HARD_TEXT_LIMIT_BYTES,
                )
                .map_err(|error| anyhow::anyhow!("managed file preview refused: {error}"))?
                .payload;
            let diff = render_unified_diff(
                &current,
                "",
                &format!("current/{path}"),
                &format!("proposed/{path}"),
            );
            return Ok(Some(ToolPreview {
                title: format!("Delete {path}"),
                summary: format!(
                    "Delete {} lines from {path}",
                    current.lines().count().max(1)
                ),
                body: diff.clone(),
                changed_files: vec![path.clone()],
                file_diffs: vec![ToolPreviewFile { path, diff }],
            }));
        }
        #[cfg(test)]
        {
            let path = required_string(&args, "path")?.to_owned();
            let target = resolve_delete_file_target(&ctx.workspace_root, &path)?;
            let current = run_blocking_io("delete_file_preview", move || {
                validate_delete_file_target(&target.path, &target.display_path)?;
                fs::read_to_string(&target.path)
                    .with_context(|| format!("failed to read {}", target.path.display()))
            })
            .await?;
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

#[cfg(not(test))]
async fn execute_managed_delete(
    tool: &DeleteFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Delete,
        )
        .map_err(|error| anyhow::anyhow!("managed file delete refused: {error}"))?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            details: json!({"path": path}),
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
                    "list-dir",
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::List,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_list(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_list(self, ctx, call_id, args).await
        }
    }
}

#[cfg(not(test))]
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
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::List,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::List {
                recursive,
                limit,
                max_depth,
            },
        )
        .map_err(|error| anyhow::anyhow!("managed file access refused: {error}"))?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            truncated: outcome.truncated,
            limit_lines: Some(limit as u64),
            returned_entries: Some(outcome.returned_entries),
            total_entries: Some(outcome.total_entries),
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
        required_string(args, "pattern")?;
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
                    "glob-pattern",
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Glob,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_glob(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_glob(self, ctx, call_id, args).await
        }
    }
}

#[cfg(not(test))]
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
    let outcome = ctx
        .execute_v3_file_operation(
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Glob,
            sigil_kernel::managed_file_access::ManagedFileExecutionInputV1::Glob { pattern, limit },
        )
        .map_err(|error| anyhow::anyhow!("managed file access refused: {error}"))?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        outcome.payload,
        ToolResultMeta {
            truncated: outcome.truncated,
            limit_lines: Some(limit as u64),
            returned_entries: Some(outcome.returned_entries),
            total_entries: Some(outcome.total_entries),
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
        required_string(args, "pattern")?;
        let path = optional_string(args, "path").unwrap_or(".");
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
                    "grep-subject",
                    sigil_kernel::managed_file_access::ManagedFileOperationV1::Grep,
                )?),
            },
        )
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        #[cfg(not(test))]
        {
            return execute_managed_grep(self, ctx, call_id, args).await;
        }
        #[cfg(test)]
        {
            execute_legacy_grep(self, ctx, call_id, args).await
        }
    }
}

#[cfg(test)]
async fn execute_legacy_read_file(
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
    let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
    // RFC-0071 R71.6: any borrowed-subject read adjudicates through the sealed V3 admission
    // before the filesystem is touched; refusal fails the tool call closed (legacy paths
    // without a V3 plan defer).
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
    let artifact_store = ctx.tool_artifact_store().cloned();
    let artifact_call_id = call_id.clone();
    let loaded = run_blocking_io("read_file", move || {
        let metadata = match fs::metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ReadFileLoad::Missing);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", resolved.display()));
            }
        };
        if !metadata.is_file() {
            return Ok(ReadFileLoad::NotAFile);
        }
        let file = File::open(&resolved)
            .with_context(|| format!("failed to open {}", resolved.display()))?;
        let mut reader = BufReader::new(file);
        let mut sink = artifact_store.as_ref().map(|store| {
            store.begin_policy_safe_capture(
                artifact_call_id,
                "read_file",
                "text/plain; charset=utf-8",
                ToolArtifactEncoding::Utf8,
                ToolArtifactSensitivity::Ordinary,
            )
        });
        let mut model_content = String::new();
        let mut returned_bytes = 0u64;
        let mut returned_lines = 0u64;
        let mut selected_source_bytes = 0u64;
        let mut selected_policy_bytes = 0u64;
        let mut selected_lines = 0usize;
        let mut total_lines = 0usize;
        let mut model_truncated = false;
        let mut model_accepting = true;
        let mut redaction_count = 0u32;
        let mut oversized_lines = 0u64;
        let model_limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);

        while let Some(line) = read_bounded_logical_line(&mut reader)? {
            let line_index = total_lines;
            total_lines = total_lines.saturating_add(1);
            if line_index < offset || selected_lines >= limit {
                continue;
            }
            let source_separator = u64::from(selected_lines > 0);
            selected_source_bytes = selected_source_bytes
                .saturating_add(source_separator)
                .saturating_add(line.observed_body_bytes);
            oversized_lines = oversized_lines.saturating_add(u64::from(line.oversized));
            let (policy_safe_line, redacted) =
                project_policy_safe_line(line, total_lines, &resolved)?;
            redaction_count = redaction_count.saturating_add(u32::from(redacted));
            let policy_separator = usize::from(selected_lines > 0);
            selected_policy_bytes = selected_policy_bytes
                .saturating_add(policy_separator as u64)
                .saturating_add(policy_safe_line.len() as u64);
            if let Some(sink) = sink.as_mut() {
                if policy_separator > 0 {
                    sink.write_all(b"\n")?;
                }
                sink.write_all(policy_safe_line.as_bytes())?;
            }
            selected_lines = selected_lines.saturating_add(1);

            let model_line = truncate_line_for_model(&policy_safe_line);
            let line_projection_truncated = model_line != policy_safe_line;
            let separator = usize::from(!model_content.is_empty());
            if model_accepting
                && model_content
                    .len()
                    .saturating_add(separator)
                    .saturating_add(model_line.len())
                    <= model_limit_bytes
            {
                if separator > 0 {
                    model_content.push('\n');
                    returned_bytes = returned_bytes.saturating_add(1);
                }
                model_content.push_str(&model_line);
                returned_bytes = returned_bytes.saturating_add(model_line.len() as u64);
                returned_lines = returned_lines.saturating_add(1);
                model_truncated |= line_projection_truncated;
            } else {
                model_truncated = true;
                model_accepting = false;
            }
        }
        let next_offset = (offset.saturating_add(selected_lines) < total_lines)
            .then_some(offset.saturating_add(selected_lines));
        let truncated = model_truncated || next_offset.is_some();
        if truncated {
            append_truncation_notice(&mut model_content);
        }
        let artifact = match sink {
            Some(sink) => {
                match sink.finish_with_source_evidence(selected_source_bytes, redaction_count) {
                    Ok(descriptor) => StreamingArtifactCapture::Published(Box::new(descriptor)),
                    Err(_) => StreamingArtifactCapture::Unavailable {
                        observed_bytes: selected_source_bytes,
                    },
                }
            }
            None => StreamingArtifactCapture::NotAttached,
        };
        Ok(ReadFileLoad::File {
            content: model_content,
            bytes: metadata.len(),
            returned_bytes,
            returned_lines,
            selected_bytes: selected_policy_bytes,
            total_lines: total_lines as u64,
            truncated,
            next_offset,
            artifact,
            oversized_lines,
        })
    })
    .await?;
    let (
        content,
        bytes,
        returned_bytes,
        returned_lines,
        selected_bytes,
        total_lines,
        truncated,
        next_offset,
        artifact,
        oversized_lines,
    ) = match loaded {
        ReadFileLoad::File {
            content,
            bytes,
            returned_bytes,
            returned_lines,
            selected_bytes,
            total_lines,
            truncated,
            next_offset,
            artifact,
            oversized_lines,
        } => (
            content,
            bytes,
            returned_bytes,
            returned_lines,
            selected_bytes,
            total_lines,
            truncated,
            next_offset,
            artifact,
            oversized_lines,
        ),
        ReadFileLoad::Missing => {
            let file_name = Path::new(&path)
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .unwrap_or("*");
            let suggested_pattern = format!("**/{file_name}");
            return Ok(ToolResult::error(
                call_id,
                tool.spec().name,
                ToolErrorKind::NotFound,
                format!(
                    "read_file path {path:?} does not exist; discover the exact workspace-relative path with glob pattern {suggested_pattern:?}; do not guess another path"
                ),
            )
            .with_error_details(
                true,
                json!({
                    "requested_path": path,
                    "recovery": "discover_path",
                    "suggested_tool": "glob",
                    "suggested_pattern": suggested_pattern,
                }),
            ));
        }
        ReadFileLoad::NotAFile => {
            return Ok(ToolResult::error(
                call_id,
                tool.spec().name,
                ToolErrorKind::InvalidInput,
                format!(
                    "read_file path {path:?} is not a regular file; use a workspace-relative file path such as src/lib.rs"
                ),
            ));
        }
    };
    let limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
    let mut details = serde_json::Map::new();
    details.insert("path".to_owned(), json!(path.as_str()));
    if let Some(language) = read_file_language(&path) {
        details.insert("language".to_owned(), json!(language));
    }
    details.insert("offset".to_owned(), json!(offset));
    if let Some(next_offset) = next_offset {
        details.insert("next_offset".to_owned(), json!(next_offset));
    }
    if oversized_lines > 0 {
        details.insert(
            "policy_omitted_oversized_lines".to_owned(),
            json!(oversized_lines),
        );
    }
    let result = ToolResult::ok(
        call_id,
        tool.spec().name,
        content,
        ToolResultMeta {
            bytes: Some(bytes),
            truncated,
            omitted_bytes: Some(selected_bytes.saturating_sub(returned_bytes)),
            limit_bytes: Some(limit_bytes as u64),
            limit_lines: Some(limit as u64),
            returned_bytes: Some(returned_bytes),
            returned_lines: Some(returned_lines),
            total_bytes: Some(selected_bytes),
            total_lines: Some(total_lines),
            details: Value::Object(details),
            ..ToolResultMeta::default()
        },
    );
    Ok(attach_streaming_artifact(result, artifact))
}

#[cfg(test)]
async fn execute_legacy_write_file(
    tool: &WriteFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let content = required_string(&args, "content")?.to_owned();
    let resolved = resolve_tool_path(&ctx.workspace_root, &path)?;
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Write,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
    let result_path = if resolved.scope == ToolSubjectScope::Workspace {
        resolved.normalized
    } else {
        path.clone()
    };
    let resolved_path = resolved.canonical;
    let bytes = content.len() as u64;
    let workspace_root = ctx.workspace_root.clone();
    let mutation_recorder = ctx.mutation_recorder.clone();
    let path_for_write = result_path.clone();
    let call_id_for_write = call_id.clone();
    run_blocking_io("write_file", move || {
        write_file_with_mutation(
            mutation_recorder.as_ref(),
            &workspace_root,
            &call_id_for_write,
            path_for_write,
            resolved_path,
            content.as_bytes(),
        )?;
        Ok(())
    })
    .await?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        format!("wrote {result_path}"),
        ToolResultMeta {
            changed_files: vec![result_path],
            bytes: Some(bytes),
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
async fn execute_legacy_edit_file(
    tool: &EditFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let old_text = required_string(&args, "old_text")?.to_owned();
    let new_text = required_string(&args, "new_text")?.to_owned();
    let resolved = resolve_tool_path(&ctx.workspace_root, &path)?;
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Edit,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
    let result_path = if resolved.scope == ToolSubjectScope::Workspace {
        resolved.normalized
    } else {
        path.clone()
    };
    let resolved_path = resolved.canonical;
    let error_path = path.clone();
    let workspace_root = ctx.workspace_root.clone();
    let mutation_recorder = ctx.mutation_recorder.clone();
    let path_for_write = result_path.clone();
    let call_id_for_write = call_id.clone();
    run_blocking_io("edit_file", move || {
        let original = fs::read_to_string(&resolved_path)
            .with_context(|| format!("failed to read {}", resolved_path.display()))?;
        let occurrences = original.matches(&old_text).count();
        if occurrences == 0 {
            bail!("old_text not found in {}", error_path);
        }
        if occurrences > 1 {
            bail!("old_text is ambiguous in {}", error_path);
        }
        let updated = original.replacen(&old_text, &new_text, 1);
        write_file_with_mutation(
            mutation_recorder.as_ref(),
            &workspace_root,
            &call_id_for_write,
            path_for_write,
            resolved_path,
            updated.as_bytes(),
        )?;
        Ok(())
    })
    .await?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        format!("edited {result_path}"),
        ToolResultMeta {
            changed_files: vec![result_path],
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
async fn execute_legacy_delete_file(
    tool: &DeleteFileTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = required_string(&args, "path")?.to_owned();
    let result_path = resolve_tool_path(&ctx.workspace_root, &path)?.normalized;
    let target = resolve_delete_file_target(&ctx.workspace_root, &path)?;
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Delete,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
    let workspace_root = ctx.workspace_root.clone();
    let mutation_recorder = ctx.mutation_recorder.clone();
    let path_for_delete = result_path.clone();
    let call_id_for_delete = call_id.clone();
    let bytes = run_blocking_io("delete_file", move || {
        let metadata = validate_delete_file_target(&target.path, &target.display_path)?;
        delete_file_with_mutation(
            mutation_recorder.as_ref(),
            &workspace_root,
            &call_id_for_delete,
            path_for_delete,
            &target.path,
        )?;
        Ok(metadata.len())
    })
    .await?;
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        format!("deleted {result_path}"),
        ToolResultMeta {
            changed_files: vec![result_path],
            bytes: Some(bytes),
            details: json!({
                "action": "delete"
            }),
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
async fn execute_legacy_list(
    tool: &ListTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let path = optional_string(&args, "path").unwrap_or(".").to_owned();
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::List,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
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
    let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
    let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
    let mut entries = run_blocking_io("ls", move || {
        let mut entries = Vec::new();
        if recursive {
            for entry in WalkBuilder::new(&resolved)
                .max_depth(Some(max_depth))
                .build()
            {
                let entry = entry?;
                entries.push(relativize(&workspace_root, entry.path())?);
            }
        } else {
            for entry in fs::read_dir(&resolved)? {
                let entry = entry?;
                entries.push(relativize(&workspace_root, &entry.path())?);
            }
        }
        Ok(entries)
    })
    .await?;
    entries.sort();
    let total_entries = entries.len();
    let truncated = total_entries > limit;
    entries.truncate(limit);
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        serde_json::to_string_pretty(&entries)?,
        ToolResultMeta {
            truncated,
            limit_lines: Some(limit as u64),
            returned_entries: Some(entries.len() as u64),
            total_entries: Some(total_entries as u64),
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
async fn execute_legacy_glob(
    tool: &GlobTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let pattern = required_string(&args, "pattern")?.to_owned();
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Glob,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
    let limit = optional_usize(&args, "limit")?
        .unwrap_or(DEFAULT_GLOB_LIMIT)
        .min(HARD_GLOB_LIMIT);
    let mut builder = GlobSetBuilder::new();
    builder.add(Glob::new(&pattern)?);
    let matcher = builder.build()?;
    let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
    let mut matches = run_blocking_io("glob", move || {
        let mut matches = Vec::new();
        for entry in WalkBuilder::new(&workspace_root).build() {
            let entry = entry?;
            let relative = relativize(&workspace_root, entry.path())?;
            if matcher.is_match(relative.as_str()) {
                matches.push(relative);
            }
        }
        Ok(matches)
    })
    .await?;
    matches.sort();
    let total_paths = matches.len();
    let truncated = total_paths > limit;
    matches.truncate(limit);
    Ok(ToolResult::ok(
        call_id,
        tool.spec().name,
        serde_json::to_string_pretty(&matches)?,
        ToolResultMeta {
            truncated,
            limit_lines: Some(limit as u64),
            returned_entries: Some(matches.len() as u64),
            total_entries: Some(total_paths as u64),
            details: json!({
                "returned_paths": matches.len(),
                "total_paths": total_paths
            }),
            ..ToolResultMeta::default()
        },
    ))
}

#[cfg(test)]
async fn execute_legacy_grep(
    tool: &GrepTool,
    ctx: ToolContext,
    call_id: String,
    args: Value,
) -> Result<ToolResult> {
    let pattern = required_string(&args, "pattern")?.to_owned();
    let root = optional_string(&args, "path").unwrap_or(".").to_owned();
    if let Err(error) = ctx.adjudicate_v3_file_operation(
        sigil_kernel::managed_file_access::ManagedFileOperationV1::Grep,
    ) {
        return Err(anyhow::anyhow!("managed file access refused: {error}"));
    }
    let limit = optional_usize(&args, "limit")?
        .unwrap_or(DEFAULT_GREP_LIMIT)
        .min(HARD_GREP_LIMIT);
    let resolved = resolve_workspace_path(&ctx.workspace_root, &root)?;
    let regex = Regex::new(&pattern)?;
    let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
    let artifact_store = ctx.tool_artifact_store().cloned();
    let artifact_call_id = call_id.clone();
    let (
        content,
        returned_matches,
        total_matches,
        binary_files_skipped,
        oversized_lines_skipped,
        artifact,
        model_output_truncated,
    ) = run_blocking_io("grep", move || {
        let model_limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
        let mut model = Vec::with_capacity(model_limit_bytes);
        model.push(b'[');
        let mut model_items = 0usize;
        let mut model_output_truncated = false;
        let mut artifact_items = 0usize;
        let mut artifact_sink = artifact_store.as_ref().map(|store| {
            store.begin_policy_safe_capture(
                artifact_call_id,
                "grep",
                "application/json",
                ToolArtifactEncoding::Utf8,
                ToolArtifactSensitivity::Ordinary,
            )
        });
        if let Some(sink) = artifact_sink.as_mut() {
            sink.write_all(b"[")?;
        }
        let mut total_matches = 0usize;
        let mut binary_files_skipped = 0usize;
        let mut oversized_lines_skipped = 0usize;
        let mut redaction_count = 0u32;
        let mut source_observed_bytes = 1u64;
        for entry in WalkBuilder::new(&resolved).build() {
            let entry = entry?;
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let file = match File::open(entry.path()) {
                Ok(file) => file,
                Err(_) => {
                    binary_files_skipped += 1;
                    continue;
                }
            };
            let mut reader = BufReader::new(file);
            let mut line_number = 0usize;
            loop {
                let line = match read_bounded_logical_line(&mut reader) {
                    Ok(Some(line)) => line,
                    Ok(None) => break,
                    Err(_) => {
                        binary_files_skipped = binary_files_skipped.saturating_add(1);
                        break;
                    }
                };
                line_number = line_number.saturating_add(1);
                if line.oversized {
                    oversized_lines_skipped = oversized_lines_skipped.saturating_add(1);
                    continue;
                }
                let Ok(text) = std::str::from_utf8(&line.bytes) else {
                    binary_files_skipped = binary_files_skipped.saturating_add(1);
                    break;
                };
                if !regex.is_match(text) {
                    continue;
                }
                total_matches = total_matches.saturating_add(1);
                let raw_match = json!({
                    "path": relativize(&workspace_root, entry.path())?,
                    "line": line_number,
                    "text": truncate_line_for_model(text),
                });
                let safe_match = safe_persistence_json_value(raw_match.clone());
                redaction_count =
                    redaction_count.saturating_add(u32::from(safe_match != raw_match));
                let raw_encoded = serde_json::to_vec(&raw_match)?;
                source_observed_bytes = source_observed_bytes
                    .saturating_add(u64::from(artifact_items > 0))
                    .saturating_add(raw_encoded.len() as u64);
                let safe_encoded = serde_json::to_vec(&safe_match)?;
                if let Some(sink) = artifact_sink.as_mut() {
                    if artifact_items > 0 {
                        sink.write_all(b",")?;
                    }
                    sink.write_all(&safe_encoded)?;
                }
                artifact_items = artifact_items.saturating_add(1);
                if model_items < limit && !model_output_truncated {
                    let separator = usize::from(model_items > 0);
                    let required = model
                        .len()
                        .saturating_add(separator)
                        .saturating_add(safe_encoded.len())
                        .saturating_add(1);
                    if required > model_limit_bytes {
                        model_output_truncated = true;
                        continue;
                    }
                    if separator > 0 {
                        model.push(b',');
                    }
                    model.extend_from_slice(&safe_encoded);
                    model_items = model_items.saturating_add(1);
                }
            }
        }
        model.push(b']');
        let content = String::from_utf8(model).context("grep model projection was not UTF-8")?;
        let artifact = match artifact_sink {
            Some(mut sink) => {
                sink.write_all(b"]")?;
                match sink.finish_with_source_evidence(
                    source_observed_bytes.saturating_add(1),
                    redaction_count,
                ) {
                    Ok(descriptor) => StreamingArtifactCapture::Published(Box::new(descriptor)),
                    Err(_) => StreamingArtifactCapture::Unavailable {
                        observed_bytes: source_observed_bytes.saturating_add(1),
                    },
                }
            }
            None => StreamingArtifactCapture::NotAttached,
        };
        Ok((
            content,
            model_items,
            total_matches,
            binary_files_skipped,
            oversized_lines_skipped,
            artifact,
            model_output_truncated,
        ))
    })
    .await?;
    let truncated = total_matches > limit || model_output_truncated;
    let content_bytes = content.len() as u64;
    let mut result = ToolResult::ok(
        call_id,
        tool.spec().name,
        content,
        ToolResultMeta {
            truncated,
            limit_bytes: Some(DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES) as u64),
            limit_lines: Some(limit as u64),
            returned_bytes: Some(content_bytes),
            returned_matches: Some(returned_matches as u64),
            total_matches: Some(total_matches as u64),
            details: json!({
                "binary_files_skipped": binary_files_skipped,
                "oversized_lines_skipped": oversized_lines_skipped,
            }),
            ..ToolResultMeta::default()
        },
    );
    result = attach_streaming_artifact(result, artifact);
    Ok(result)
}
