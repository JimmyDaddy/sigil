use std::{
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{Value, json};
use similar::TextDiff;
use termquill_kernel::{
    Tool, ToolContext, ToolPreview, ToolPreviewFile, ToolRegistry, ToolResult, ToolResultMeta,
    ToolSpec,
};
use tokio::{process::Command, task, time::Duration};

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(ListTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry.register(Arc::new(BashTool));
}

struct ReadFileTool;
struct WriteFileTool;
struct EditFileTool;
struct ListTool;
struct GlobTool;
struct GrepTool;
struct BashTool;

#[async_trait]
impl Tool for ReadFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read a UTF-8 text file from the workspace.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
            read_only: true,
        }
    }

    fn permission_subject(&self, args: &Value) -> Result<Option<String>> {
        let path = required_string(args, "path")?;
        Ok(Some(normalized_relative_path_string(path)?))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
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
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content,
            is_error: false,
            metadata: ToolResultMeta {
                bytes: Some(bytes),
                ..ToolResultMeta::default()
            },
        })
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".to_owned(),
            description: "Write UTF-8 content to a workspace file.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
            read_only: false,
        }
    }

    fn permission_subject(&self, args: &Value) -> Result<Option<String>> {
        let path = required_string(args, "path")?;
        Ok(Some(normalized_relative_path_string(path)?))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
        let content = required_string(&args, "content")?.to_owned();
        let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
        let result_path = resolved.display().to_string();
        let bytes = content.len() as u64;
        run_blocking_io("write_file", move || {
            if let Some(parent) = resolved.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::write(&resolved, content.as_bytes())
                .with_context(|| format!("failed to write {}", resolved.display()))?;
            Ok(())
        })
        .await?;
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content: format!("wrote {result_path}"),
            is_error: false,
            metadata: ToolResultMeta {
                changed_files: vec![path.to_owned()],
                bytes: Some(bytes),
                ..ToolResultMeta::default()
            },
        })
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
            read_only: false,
        }
    }

    fn permission_subject(&self, args: &Value) -> Result<Option<String>> {
        let path = required_string(args, "path")?;
        Ok(Some(normalized_relative_path_string(path)?))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = required_string(&args, "path")?.to_owned();
        let old_text = required_string(&args, "old_text")?.to_owned();
        let new_text = required_string(&args, "new_text")?.to_owned();
        let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
        let result_path = resolved.display().to_string();
        let error_path = path.clone();
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
            fs::write(&resolved, updated.as_bytes())
                .with_context(|| format!("failed to edit {}", resolved.display()))?;
            Ok(())
        })
        .await?;
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content: format!("edited {result_path}"),
            is_error: false,
            metadata: ToolResultMeta {
                changed_files: vec![path.to_owned()],
                ..ToolResultMeta::default()
            },
        })
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
impl Tool for ListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "ls".to_owned(),
            description: "List files and directories inside the workspace.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "recursive": { "type": "boolean" }
                }
            }),
            read_only: true,
        }
    }

    fn permission_subject(&self, args: &Value) -> Result<Option<String>> {
        let path = optional_string(args, "path").unwrap_or(".");
        Ok(Some(normalized_relative_path_string(path)?))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let path = optional_string(&args, "path").unwrap_or(".").to_owned();
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let resolved = resolve_workspace_path(&ctx.workspace_root, &path)?;
        let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
        let mut entries = run_blocking_io("ls", move || {
            let mut entries = Vec::new();
            if recursive {
                for entry in WalkBuilder::new(&resolved).build() {
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
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content: serde_json::to_string_pretty(&entries)?,
            is_error: false,
            metadata: ToolResultMeta::default(),
        })
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
                "properties": { "pattern": { "type": "string" } },
                "required": ["pattern"]
            }),
            read_only: true,
        }
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let pattern = required_string(&args, "pattern")?.to_owned();
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
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content: serde_json::to_string_pretty(&matches)?,
            is_error: false,
            metadata: ToolResultMeta::default(),
        })
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
                    "path": { "type": "string" }
                },
                "required": ["pattern"]
            }),
            read_only: true,
        }
    }

    fn permission_subject(&self, args: &Value) -> Result<Option<String>> {
        let path = optional_string(args, "path").unwrap_or(".");
        Ok(Some(normalized_relative_path_string(path)?))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let pattern = required_string(&args, "pattern")?.to_owned();
        let root = optional_string(&args, "path").unwrap_or(".").to_owned();
        let resolved = resolve_workspace_path(&ctx.workspace_root, &root)?;
        let regex = Regex::new(&pattern)?;
        let workspace_root = canonical_workspace_root(&ctx.workspace_root)?;
        let matches = run_blocking_io("grep", move || {
            let mut matches = Vec::new();
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
                    Err(_) => continue,
                };
                for (index, line) in content.lines().enumerate() {
                    if regex.is_match(line) {
                        matches.push(json!({
                            "path": relativize(&workspace_root, entry.path())?,
                            "line": index + 1,
                            "text": line,
                        }));
                    }
                }
            }
            Ok(matches)
        })
        .await?;
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content: serde_json::to_string_pretty(&matches)?,
            is_error: false,
            metadata: ToolResultMeta::default(),
        })
    }
}

#[async_trait]
impl Tool for BashTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "bash".to_owned(),
            description: "Run a shell command from the workspace root.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" },
                    "timeout_secs": { "type": "integer" }
                },
                "required": ["command"]
            }),
            read_only: false,
        }
    }

    fn permission_subject(&self, args: &Value) -> Result<Option<String>> {
        let command = required_string(args, "command")?;
        Ok(Some(command_permission_subject(command)))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let command = required_string(&args, "command")?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(ctx.timeout_secs);
        let mut child = Command::new("sh");
        child
            .arg("-lc")
            .arg(command)
            .current_dir(&ctx.workspace_root);
        let output = tokio::time::timeout(Duration::from_secs(timeout_secs), child.output())
            .await
            .context("bash command timed out")??;
        let mut content = String::new();
        if !output.stdout.is_empty() {
            content.push_str(&String::from_utf8_lossy(&output.stdout));
        }
        if !output.stderr.is_empty() {
            if !content.is_empty() {
                content.push('\n');
            }
            content.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        Ok(ToolResult {
            call_id,
            tool_name: self.spec().name,
            content,
            is_error: !output.status.success(),
            metadata: ToolResultMeta {
                exit_code: output.status.code(),
                ..ToolResultMeta::default()
            },
        })
    }
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

fn optional_string<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn resolve_workspace_path(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    let relative = normalize_workspace_relative_path(requested)?;
    let workspace_root = canonical_workspace_root(workspace_root)?;
    resolve_confined_path(&workspace_root, &relative)
}

fn normalize_workspace_relative_path(requested: &str) -> Result<PathBuf> {
    let requested_path = Path::new(requested);
    if requested_path.is_absolute() {
        bail!("absolute paths are not allowed");
    }

    let mut normalized = PathBuf::new();
    for component in requested_path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => bail!("parent-directory traversal is not allowed"),
            Component::RootDir | Component::Prefix(_) => {
                bail!("absolute paths are not allowed")
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(normalized)
    }
}

fn normalized_relative_path_string(requested: &str) -> Result<String> {
    Ok(normalize_workspace_relative_path(requested)?
        .to_string_lossy()
        .to_string())
}

fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace root {}",
            workspace_root.display()
        )
    })
}

fn resolve_confined_path(workspace_root: &Path, relative: &Path) -> Result<PathBuf> {
    if relative == Path::new(".") {
        return Ok(workspace_root.to_path_buf());
    }

    let mut resolved = workspace_root.to_path_buf();
    let components = relative
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_os_string()),
            Component::CurDir => None,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => None,
        })
        .collect::<Vec<OsString>>();

    for (index, component) in components.iter().enumerate() {
        let candidate = resolved.join(component);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                let canonical = fs::canonicalize(&candidate)
                    .with_context(|| format!("failed to resolve {}", candidate.display()))?;
                ensure_inside_workspace(workspace_root, &canonical)?;
                resolved = canonical;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                resolved = candidate;
                for remaining in components.iter().skip(index + 1) {
                    resolved.push(remaining);
                }
                ensure_inside_workspace(workspace_root, &resolved)?;
                return Ok(resolved);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", candidate.display()));
            }
        }
    }

    ensure_inside_workspace(workspace_root, &resolved)?;
    Ok(resolved)
}

fn ensure_inside_workspace(workspace_root: &Path, path: &Path) -> Result<()> {
    if path.starts_with(workspace_root) {
        Ok(())
    } else {
        bail!(
            "path {} escapes workspace {}",
            path.display(),
            workspace_root.display()
        );
    }
}

fn relativize(workspace_root: &Path, path: &Path) -> Result<String> {
    Ok(path
        .strip_prefix(workspace_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string())
}

fn command_permission_subject(command: &str) -> String {
    const MAX_CHARS: usize = 120;
    let normalized = command.split_whitespace().collect::<Vec<_>>().join(" ");
    let char_count = normalized.chars().count();
    if char_count <= MAX_CHARS {
        return normalized;
    }
    let truncated = normalized.chars().take(MAX_CHARS).collect::<String>();
    format!("{truncated}...")
}

fn render_unified_diff(
    current: &str,
    proposed: &str,
    current_label: &str,
    proposed_label: &str,
) -> String {
    let diff = TextDiff::from_lines(current, proposed)
        .unified_diff()
        .context_radius(2)
        .header(current_label, proposed_label)
        .to_string();

    if diff.trim().is_empty() {
        "No textual changes detected.".to_owned()
    } else {
        diff
    }
}

async fn run_blocking_io<T, F>(label: &'static str, job: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    task::spawn_blocking(job)
        .await
        .with_context(|| format!("{label} blocking task failed to join"))?
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;
    use serde_json::json;
    use termquill_kernel::{Tool, ToolContext, ToolRegistry};

    use super::{
        BashTool, EditFileTool, GlobTool, GrepTool, ListTool, ReadFileTool, WriteFileTool,
        register_builtin_tools,
    };

    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[tokio::test]
    async fn read_and_edit_file_tool_work() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("note.txt");
        fs::write(&file, "hello old")?;
        let ctx = ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        };
        let read = ReadFileTool
            .execute(ctx.clone(), "1".to_owned(), json!({ "path": "note.txt" }))
            .await?;
        assert_eq!(read.content, "hello old");
        EditFileTool
            .execute(
                ctx.clone(),
                "2".to_owned(),
                json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
            )
            .await?;
        assert_eq!(fs::read_to_string(file)?, "hello new");
        Ok(())
    }

    #[tokio::test]
    async fn write_file_preview_contains_diff() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("note.txt");
        fs::write(&file, "alpha\nbeta\n")?;
        let ctx = ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        };
        let preview = WriteFileTool
            .preview(
                ctx,
                json!({ "path": "note.txt", "content": "alpha\nbeta\ngamma\n" }),
            )
            .await?
            .expect("expected preview");
        assert!(preview.body.contains("--- current/note.txt"));
        assert!(preview.body.contains("+++ proposed/note.txt"));
        assert!(preview.body.contains("+gamma"));
        Ok(())
    }

    #[tokio::test]
    async fn write_file_preview_errors_for_unreadable_existing_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("note.txt");
        fs::write(&file, [0xff_u8, 0xfe, 0xfd])?;
        let ctx = ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        };
        let error = WriteFileTool
            .preview(
                ctx,
                json!({ "path": "note.txt", "content": "hello\nworld\n" }),
            )
            .await
            .expect_err("expected preview generation to surface the read failure");
        assert!(error.to_string().contains("failed to read"));
        Ok(())
    }

    #[tokio::test]
    async fn edit_file_preview_contains_replacement() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let file = temp.path().join("note.txt");
        fs::write(&file, "hello old\n")?;
        let ctx = ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        };
        let preview = EditFileTool
            .preview(
                ctx,
                json!({ "path": "note.txt", "old_text": "old", "new_text": "new" }),
            )
            .await?
            .expect("expected preview");
        assert!(preview.body.contains("-hello old"));
        assert!(preview.body.contains("+hello new"));
        Ok(())
    }

    #[test]
    fn register_builtin_tools_registers_multiple_tools() {
        let mut registry = ToolRegistry::new();
        register_builtin_tools(&mut registry);
        assert!(registry.specs().len() >= 7);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn read_file_rejects_symlink_escape() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "secret")?;
        symlink(&outside_file, workspace.path().join("leak.txt"))?;
        let ctx = ToolContext {
            workspace_root: workspace.path().to_path_buf(),
            timeout_secs: 5,
        };

        let error = ReadFileTool
            .execute(ctx, "read".to_owned(), json!({ "path": "leak.txt" }))
            .await
            .expect_err("expected symlink escape to be rejected");

        assert!(error.to_string().contains("escapes workspace"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_rejects_existing_symlink_escape() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "secret")?;
        symlink(&outside_file, workspace.path().join("leak.txt"))?;
        let ctx = ToolContext {
            workspace_root: workspace.path().to_path_buf(),
            timeout_secs: 5,
        };

        let error = WriteFileTool
            .execute(
                ctx,
                "write".to_owned(),
                json!({ "path": "leak.txt", "content": "changed" }),
            )
            .await
            .expect_err("expected symlink escape to be rejected");

        assert!(error.to_string().contains("escapes workspace"));
        assert_eq!(fs::read_to_string(outside_file)?, "secret");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_file_rejects_symlink_parent_escape_for_new_file() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        symlink(outside.path(), workspace.path().join("outside-dir"))?;
        let ctx = ToolContext {
            workspace_root: workspace.path().to_path_buf(),
            timeout_secs: 5,
        };

        let error = WriteFileTool
            .execute(
                ctx,
                "write".to_owned(),
                json!({ "path": "outside-dir/new.txt", "content": "changed" }),
            )
            .await
            .expect_err("expected symlink parent escape to be rejected");

        assert!(error.to_string().contains("escapes workspace"));
        assert!(!outside.path().join("new.txt").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn edit_file_rejects_symlink_escape() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        let outside_file = outside.path().join("secret.txt");
        fs::write(&outside_file, "hello old")?;
        symlink(&outside_file, workspace.path().join("leak.txt"))?;
        let ctx = ToolContext {
            workspace_root: workspace.path().to_path_buf(),
            timeout_secs: 5,
        };

        let error = EditFileTool
            .execute(
                ctx,
                "edit".to_owned(),
                json!({ "path": "leak.txt", "old_text": "old", "new_text": "new" }),
            )
            .await
            .expect_err("expected symlink escape to be rejected");

        assert!(error.to_string().contains("escapes workspace"));
        assert_eq!(fs::read_to_string(outside_file)?, "hello old");
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_and_grep_reject_external_symlink_roots() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("secret.txt"), "secret")?;
        symlink(outside.path(), workspace.path().join("outside-dir"))?;
        let ctx = ToolContext {
            workspace_root: workspace.path().to_path_buf(),
            timeout_secs: 5,
        };

        let list_error = ListTool
            .execute(
                ctx.clone(),
                "list".to_owned(),
                json!({ "path": "outside-dir", "recursive": true }),
            )
            .await
            .expect_err("expected list symlink root to be rejected");
        let grep_error = GrepTool
            .execute(
                ctx,
                "grep".to_owned(),
                json!({ "path": "outside-dir", "pattern": "secret" }),
            )
            .await
            .expect_err("expected grep symlink root to be rejected");

        assert!(list_error.to_string().contains("escapes workspace"));
        assert!(grep_error.to_string().contains("escapes workspace"));
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn glob_does_not_traverse_external_symlink_targets() -> Result<()> {
        let workspace = tempfile::tempdir()?;
        let outside = tempfile::tempdir()?;
        fs::write(outside.path().join("secret.txt"), "secret")?;
        symlink(outside.path(), workspace.path().join("outside-dir"))?;
        fs::write(workspace.path().join("visible.txt"), "visible")?;
        let ctx = ToolContext {
            workspace_root: workspace.path().to_path_buf(),
            timeout_secs: 5,
        };

        let result = GlobTool
            .execute(ctx, "glob".to_owned(), json!({ "pattern": "**/*.txt" }))
            .await?;

        assert!(result.content.contains("visible.txt"));
        assert!(!result.content.contains("secret.txt"));
        Ok(())
    }

    #[tokio::test]
    async fn bash_tool_timeout_surfaces_structured_error() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let ctx = ToolContext {
            workspace_root: temp.path().to_path_buf(),
            timeout_secs: 5,
        };

        let error = BashTool
            .execute(
                ctx,
                "bash".to_owned(),
                json!({ "command": "sleep 2", "timeout_secs": 1 }),
            )
            .await
            .expect_err("expected timeout to be surfaced");

        assert!(error.to_string().contains("bash command timed out"));
        Ok(())
    }
}
