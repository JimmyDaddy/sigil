use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{Value, json};
use sigil_kernel::{
    Tool, ToolAccess, ToolCategory, ToolContext, ToolOperation, ToolPreview, ToolPreviewCapability,
    ToolPreviewFile, ToolResult, ToolResultMeta, ToolSpec, ToolSubject, ToolSubjectScope,
    delete_file_with_mutation, write_file_with_mutation,
};

use crate::{
    constants::{
        DEFAULT_GLOB_LIMIT, DEFAULT_GREP_LIMIT, DEFAULT_LIST_LIMIT, DEFAULT_READ_LIMIT_LINES,
        DEFAULT_RECURSIVE_LIST_LIMIT, DEFAULT_RECURSIVE_MAX_DEPTH, DEFAULT_TEXT_LIMIT_BYTES,
        HARD_GLOB_LIMIT, HARD_GREP_LIMIT, HARD_LIST_LIMIT, HARD_READ_LIMIT_LINES,
        HARD_TEXT_LIMIT_BYTES, SIGIL_SCRATCH_DIR_ENV,
    },
    path::{
        canonical_workspace_root, lexically_normalize_path, relativize, resolve_delete_file_target,
        resolve_tool_path_from_base, resolve_workspace_path, tool_path_subject,
        validate_delete_file_target,
    },
    support::{
        limit_text_head, optional_string, optional_usize, render_unified_diff, required_string,
        run_blocking_io, truncate_line_for_model,
    },
};

pub(crate) struct ReadFileTool;
pub(crate) struct WriteFileTool;
pub(crate) struct EditFileTool;
pub(crate) struct DeleteFileTool;

pub(crate) struct ListTool;
pub(crate) struct GlobTool;
pub(crate) struct GrepTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read a UTF-8 text file from the workspace.".to_owned(),
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

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let path = required_string(args, "path")?;
        Ok(vec![tool_path_subject(&ctx.workspace_root, path)?])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
        let offset = optional_usize(&args, "offset")?.unwrap_or(0);
        let limit = optional_usize(&args, "limit")?
            .unwrap_or(DEFAULT_READ_LIMIT_LINES)
            .min(HARD_READ_LIMIT_LINES);
        let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
        let (content, bytes) = run_blocking_io("read_file", move || {
            let content = fs::read_to_string(&resolved)
                .with_context(|| format!("failed to read {}", resolved.display()))?;
            let bytes = fs::metadata(&resolved)
                .with_context(|| format!("failed to inspect {}", resolved.display()))?
                .len();
            Ok((content, bytes))
        })
        .await?;
        let total_lines = content.lines().count();
        let selected = content.lines().skip(offset).collect::<Vec<_>>().join("\n");
        let limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
        let limited = limit_text_head(&selected, limit_bytes, limit);
        let next_offset = offset + limited.returned_lines as usize;
        let mut details = serde_json::Map::new();
        details.insert("path".to_owned(), json!(path.as_str()));
        if let Some(language) = read_file_language(&path) {
            details.insert("language".to_owned(), json!(language));
        }
        details.insert("offset".to_owned(), json!(offset));
        if next_offset < total_lines {
            details.insert("next_offset".to_owned(), json!(next_offset));
        }
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            limited.content,
            ToolResultMeta {
                bytes: Some(bytes),
                truncated: limited.truncated || next_offset < total_lines,
                omitted_bytes: Some(limited.omitted_bytes),
                limit_bytes: Some(limit_bytes as u64),
                limit_lines: Some(limit as u64),
                returned_bytes: Some(limited.returned_bytes),
                returned_lines: Some(limited.returned_lines),
                total_bytes: Some(limited.total_bytes),
                total_lines: Some(total_lines as u64),
                details: Value::Object(details),
                ..ToolResultMeta::default()
            },
        ))
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

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let path = required_string(args, "path")?;
        Ok(vec![tool_path_subject(&ctx.workspace_root, path)?])
    }

    fn permission_operation(&self, ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let path = required_string(args, "path")?;
        let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
        let requested_path = Path::new(path);
        let target = if requested_path.is_absolute() {
            lexically_normalize_path(requested_path)?
        } else {
            lexically_normalize_path(&workspace_root.join(requested_path))?
        };
        let resolved = resolve_tool_path_from_base(&workspace_root, &workspace_root, path)?;
        if resolved.scope != ToolSubjectScope::Workspace {
            bail!("write_file path is outside workspace: {path}");
        }
        if target.exists() {
            Ok(ToolOperation::OverwriteFile)
        } else {
            Ok(ToolOperation::CreateFile)
        }
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
        let content = required_string(&args, "content")?.to_owned();
        let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
        let result_path = resolved.display().to_string();
        let bytes = content.len() as u64;
        let workspace_root = ctx.workspace_root.clone();
        let mutation_recorder = ctx.mutation_recorder.clone();
        let path_for_write = path.clone();
        let call_id_for_write = call_id.clone();
        run_blocking_io("write_file", move || {
            write_file_with_mutation(
                mutation_recorder.as_ref(),
                &workspace_root,
                &call_id_for_write,
                path_for_write,
                resolved,
                content.as_bytes(),
            )?;
            Ok(())
        })
        .await?;
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            format!("wrote {result_path}"),
            ToolResultMeta {
                changed_files: vec![path.to_owned()],
                bytes: Some(bytes),
                ..ToolResultMeta::default()
            },
        ))
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
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

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let path = required_string(args, "path")?;
        Ok(vec![tool_path_subject(&ctx.workspace_root, path)?])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
        let old_text = required_string(&args, "old_text")?.to_owned();
        let new_text = required_string(&args, "new_text")?.to_owned();
        let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
        let result_path = resolved.display().to_string();
        let error_path = path.clone();
        let workspace_root = ctx.workspace_root.clone();
        let mutation_recorder = ctx.mutation_recorder.clone();
        let path_for_write = path.clone();
        let call_id_for_write = call_id.clone();
        run_blocking_io("edit_file", move || {
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
            write_file_with_mutation(
                mutation_recorder.as_ref(),
                &workspace_root,
                &call_id_for_write,
                path_for_write,
                resolved,
                updated.as_bytes(),
            )?;
            Ok(())
        })
        .await?;
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            format!("edited {result_path}"),
            ToolResultMeta {
                changed_files: vec![path.to_owned()],
                ..ToolResultMeta::default()
            },
        ))
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
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

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let path = required_string(args, "path")?;
        Ok(vec![tool_path_subject(&ctx.workspace_root, path)?])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
        let target = resolve_delete_file_target(&ctx.workspace_root, &path)?;
        let result_path = target.path.display().to_string();
        let workspace_root = ctx.workspace_root.clone();
        let mutation_recorder = ctx.mutation_recorder.clone();
        let path_for_delete = path.clone();
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
            self.spec().name,
            format!("deleted {result_path}"),
            ToolResultMeta {
                changed_files: vec![path],
                bytes: Some(bytes),
                details: json!({
                    "action": "delete"
                }),
                ..ToolResultMeta::default()
            },
        ))
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
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

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let path = optional_string(args, "path").unwrap_or(".");
        Ok(vec![tool_path_subject(&ctx.workspace_root, path)?])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = optional_string(&args, "path").unwrap_or(".").to_owned();
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
            self.spec().name,
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

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let pattern = required_string(&args, "pattern")?.to_owned();
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
            self.spec().name,
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

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let path = optional_string(args, "path").unwrap_or(".");
        Ok(vec![tool_path_subject(&ctx.workspace_root, path)?])
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let pattern = required_string(&args, "pattern")?.to_owned();
        let root = optional_string(&args, "path").unwrap_or(".").to_owned();
        let limit = optional_usize(&args, "limit")?
            .unwrap_or(DEFAULT_GREP_LIMIT)
            .min(HARD_GREP_LIMIT);
        let resolved = resolve_workspace_path(&ctx.workspace_root, &root)?;
        let regex = Regex::new(&pattern)?;
        let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
        let (mut matches, binary_files_skipped) = run_blocking_io("grep", move || {
            let mut matches = Vec::new();
            let mut binary_files_skipped = 0usize;
            for entry in WalkBuilder::new(&resolved).build() {
                let entry = entry?;
                if !entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
                {
                    continue;
                }
                let content = match fs::read_to_string(entry.path()) {
                    Ok(content) => content,
                    Err(_) => {
                        binary_files_skipped += 1;
                        continue;
                    }
                };
                for (index, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        matches.push(json!({
                            "path": relativize(&workspace_root, entry.path())?,
                            "line": index + 1,
                            "text": truncate_line_for_model(line),
                        }));
                    }
                }
            }
            Ok((matches, binary_files_skipped))
        })
        .await?;
        let total_matches = matches.len();
        let truncated = total_matches > limit;
        matches.truncate(limit);
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            serde_json::to_string_pretty(&matches)?,
            ToolResultMeta {
                truncated,
                limit_lines: Some(limit as u64),
                returned_matches: Some(matches.len() as u64),
                total_matches: Some(total_matches as u64),
                details: json!({
                    "binary_files_skipped": binary_files_skipped
                }),
                ..ToolResultMeta::default()
            },
        ))
    }
}
