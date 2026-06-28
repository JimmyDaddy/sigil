use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex as StdMutex},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk, ChangeSetValidation,
    ChangeSetValidationKind, ChangeSetValidationStatus, CommittedFileMutation, ExecutionBackend,
    ExecutionBackendCapabilities, ExecutionBackendKind, ExecutionConfig, ExecutionFuture,
    ExecutionReceipt, ExecutionRequest, FileType, MutationBatchId, MutationBatchStatus,
    MutationEventRecorder, MutationSubject, TerminalTaskEntry, TerminalTaskId, Tool, ToolAccess,
    ToolCategory, ToolContext, ToolDiffStats, ToolErrorKind, ToolOperation, ToolPreview,
    ToolPreviewCapability, ToolPreviewFile, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, ToolSubjectScope, delete_file_with_mutation, delete_file_with_mutation_in_batch,
    write_file_with_mutation, write_file_with_mutation_in_batch,
};
use similar::TextDiff;
use tokio::{process::Command, task, time::Duration};

mod terminal_process;

pub use terminal_process::{
    MAX_TERMINAL_INPUT_BYTES, TerminalBackendKind, TerminalInputResult, TerminalProcessManager,
    TerminalPtySize, TerminalReadResult, TerminalResizeResult, TerminalStartRequest,
    TerminalTaskArtifacts, TerminalTaskPermissionContext,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinToolPaths {
    pub changesets_root: PathBuf,
    pub changesets_label_root: PathBuf,
    pub terminal_tasks_root: PathBuf,
    pub terminal_tasks_label_root: PathBuf,
    pub scratch_root: PathBuf,
    pub scratch_label: String,
}

impl BuiltinToolPaths {
    #[must_use]
    pub fn workspace_defaults(workspace_root: &Path) -> Self {
        Self {
            changesets_root: workspace_root.join(CHANGESET_ARTIFACT_ROOT),
            changesets_label_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            terminal_tasks_root: workspace_root.join(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            terminal_tasks_label_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            scratch_root: workspace_root.join(WORKSPACE_TEMP_ROOT),
            scratch_label: WORKSPACE_TEMP_ROOT.to_owned(),
        }
    }
}

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    register_builtin_tools_with_paths(
        registry,
        BuiltinToolPaths {
            changesets_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            changesets_label_root: PathBuf::from(CHANGESET_ARTIFACT_ROOT),
            terminal_tasks_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            terminal_tasks_label_root: PathBuf::from(terminal_process::TERMINAL_TASK_ARTIFACT_ROOT),
            scratch_root: PathBuf::from(WORKSPACE_TEMP_ROOT),
            scratch_label: WORKSPACE_TEMP_ROOT.to_owned(),
        },
    );
}

pub fn register_builtin_tools_with_paths(registry: &mut ToolRegistry, paths: BuiltinToolPaths) {
    register_builtin_tools_with_paths_and_execution_backend(
        registry,
        paths,
        Arc::new(LocalExecutionBackend),
    );
}

pub fn register_builtin_tools_with_paths_and_execution_backend(
    registry: &mut ToolRegistry,
    paths: BuiltinToolPaths,
    execution_backend: Arc<dyn ExecutionBackend>,
) {
    let terminal_managers = Arc::new(TerminalProcessManagers::default());
    let terminal_tasks_root = paths.terminal_tasks_root;
    let terminal_tasks_label_root = paths.terminal_tasks_label_root;
    registry.register(Arc::new(ReadFileTool));
    registry.register(Arc::new(WriteFileTool));
    registry.register(Arc::new(EditFileTool));
    registry.register(Arc::new(DeleteFileTool));
    registry.register(Arc::new(ApplyChangeSetTool {
        artifact_root: paths.changesets_root,
        artifact_label_root: paths.changesets_label_root,
    }));
    registry.register(Arc::new(ListTool));
    registry.register(Arc::new(GlobTool));
    registry.register(Arc::new(GrepTool));
    registry.register(Arc::new(BashTool {
        scratch_root: paths.scratch_root.clone(),
        scratch_label: paths.scratch_label.clone(),
        backend: Arc::clone(&execution_backend),
    }));
    registry.register(Arc::new(TerminalStartTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
        scratch_root: paths.scratch_root,
        scratch_label: paths.scratch_label,
    }));
    registry.register(Arc::new(TerminalReadTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
    }));
    registry.register(Arc::new(TerminalInputTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
    }));
    registry.register(Arc::new(TerminalResizeTool {
        managers: Arc::clone(&terminal_managers),
        artifact_root: terminal_tasks_root.clone(),
        artifact_label_root: terminal_tasks_label_root.clone(),
    }));
    registry.register(Arc::new(TerminalCancelTool {
        artifact_root: terminal_tasks_root,
        artifact_label_root: terminal_tasks_label_root,
        managers: terminal_managers,
    }));
}

struct ReadFileTool;
struct WriteFileTool;
struct EditFileTool;
struct DeleteFileTool;
struct ApplyChangeSetTool {
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
}
struct ListTool;
struct GlobTool;
struct GrepTool;
struct BashTool {
    scratch_root: PathBuf,
    scratch_label: String,
    backend: Arc<dyn ExecutionBackend>,
}
struct TerminalStartTool {
    managers: Arc<TerminalProcessManagers>,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
    scratch_root: PathBuf,
    scratch_label: String,
}
struct TerminalReadTool {
    managers: Arc<TerminalProcessManagers>,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
}
struct TerminalInputTool {
    managers: Arc<TerminalProcessManagers>,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
}
struct TerminalResizeTool {
    managers: Arc<TerminalProcessManagers>,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
}
struct TerminalCancelTool {
    managers: Arc<TerminalProcessManagers>,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
}

#[derive(Debug, Default)]
pub struct LocalExecutionBackend;

impl ExecutionBackend for LocalExecutionBackend {
    fn kind(&self) -> ExecutionBackendKind {
        ExecutionBackendKind::Local
    }

    fn capabilities(&self) -> ExecutionBackendCapabilities {
        ExecutionBackendCapabilities::default()
    }

    fn execute(&self, request: ExecutionRequest) -> ExecutionFuture<'_> {
        Box::pin(local_execute(self.kind(), self.capabilities(), request))
    }
}

/// Builds the configured execution backend for built-in tools.
///
/// # Errors
///
/// Returns an error when configuration requires sandbox enforcement that the selected backend
/// cannot provide.
pub fn build_execution_backend(config: &ExecutionConfig) -> Result<Arc<dyn ExecutionBackend>> {
    let backend: Arc<dyn ExecutionBackend> = match config.backend {
        ExecutionBackendKind::Local => Arc::new(LocalExecutionBackend),
    };
    validate_execution_backend(config, backend.as_ref())?;
    Ok(backend)
}

fn validate_execution_backend(
    config: &ExecutionConfig,
    backend: &dyn ExecutionBackend,
) -> Result<()> {
    if config.isolation.requires_sandbox() && !backend.capabilities().supports_required_sandbox() {
        bail!(
            "execution sandbox required but {:?} backend does not provide filesystem and process isolation",
            backend.kind()
        );
    }
    Ok(())
}

async fn local_execute(
    backend: ExecutionBackendKind,
    capabilities: ExecutionBackendCapabilities,
    request: ExecutionRequest,
) -> Result<ExecutionReceipt> {
    let mut command = Command::new(&request.program);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .envs(&request.env)
        .kill_on_drop(true);

    let output =
        match tokio::time::timeout(Duration::from_secs(request.timeout_secs), command.output())
            .await
        {
            Ok(output) => output?,
            Err(_) => {
                return Ok(ExecutionReceipt {
                    backend,
                    capabilities,
                    exit_code: None,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    timed_out: true,
                });
            }
        };

    Ok(ExecutionReceipt {
        backend,
        capabilities,
        exit_code: output.status.code(),
        stdout: output.stdout,
        stderr: output.stderr,
        timed_out: false,
    })
}

#[derive(Default)]
struct TerminalProcessManagers {
    managers: StdMutex<BTreeMap<(PathBuf, PathBuf), Arc<TerminalProcessManager>>>,
}

const DEFAULT_TEXT_LIMIT_BYTES: usize = 64 * 1024;
const HARD_TEXT_LIMIT_BYTES: usize = 256 * 1024;
const DEFAULT_READ_LIMIT_LINES: usize = 1000;
const HARD_READ_LIMIT_LINES: usize = 2000;
const MAX_MODEL_LINE_CHARS: usize = 2000;
const DEFAULT_LIST_LIMIT: usize = 200;
const DEFAULT_RECURSIVE_LIST_LIMIT: usize = 500;
const HARD_LIST_LIMIT: usize = 2000;
const DEFAULT_RECURSIVE_MAX_DEPTH: usize = 3;
const DEFAULT_GLOB_LIMIT: usize = 100;
const HARD_GLOB_LIMIT: usize = 1000;
const DEFAULT_GREP_LIMIT: usize = 100;
const HARD_GREP_LIMIT: usize = 1000;
const CHANGESET_ARTIFACT_ROOT: &str = "state/artifacts/changesets";
const WORKSPACE_TEMP_ROOT: &str = "cache/tmp";
const CHANGESET_PREVIEW_DIFF_FILE: &str = "preview.diff";
const CHANGESET_REVERSE_DIFF_FILE: &str = "reverse.diff";
const DEFAULT_CHANGESET_SUMMARY_LIMIT_BYTES: usize = 16 * 1024;
const DEFAULT_TERMINAL_READ_LIMIT_BYTES: usize = 16 * 1024;
const HARD_TERMINAL_READ_LIMIT_BYTES: usize = 128 * 1024;
const SIGIL_SCRATCH_DIR_ENV: &str = "SIGIL_SCRATCH_DIR";

impl TerminalProcessManagers {
    fn manager_for(
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

        let manager = Arc::new(TerminalProcessManager::new_with_artifact_root(
            &workspace_root,
            artifact_root,
            artifact_label_root.to_path_buf(),
        )?);
        managers.insert(key, Arc::clone(&manager));
        Ok(manager)
    }
}

/// Artifact writer for durable change set diffs.
#[derive(Debug, Clone)]
pub struct ChangeSetArtifactStore {
    workspace_root: PathBuf,
    artifact_root: PathBuf,
    artifact_label_root: PathBuf,
    summary_limit_bytes: usize,
}

/// Durable metadata for one stored change set artifact set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetArtifactRecord {
    pub change_set_id: ChangeSetId,
    pub artifact_dir: String,
    pub preview: ChangeSetDiffArtifact,
    pub reverse: ChangeSetDiffArtifact,
    pub summary: ChangeSetArtifactSummary,
}

/// Bounded metadata for one diff artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetDiffArtifact {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub line_count: u64,
    pub stats: ToolDiffStats,
}

/// Bounded preview summary suitable for append-only control entries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ChangeSetArtifactSummary {
    pub text: String,
    pub truncated: bool,
    pub returned_bytes: u64,
    pub omitted_bytes: u64,
    pub total_bytes: u64,
    pub total_lines: u64,
    pub limit_bytes: u64,
}

#[derive(Debug, Clone)]
struct ChangeSetArtifactPaths {
    relative_dir: String,
    relative_preview: String,
    relative_reverse: String,
    absolute_dir: PathBuf,
    absolute_preview: PathBuf,
    absolute_reverse: PathBuf,
}

impl ChangeSetArtifactStore {
    /// Creates a store rooted at `<workspace>/state/artifacts/changesets`.
    ///
    /// # Errors
    ///
    /// Returns an error when `workspace_root` cannot be canonicalized.
    pub fn new(workspace_root: impl AsRef<Path>) -> Result<Self> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        Self::new_with_artifact_root(
            &workspace_root,
            workspace_root.join(CHANGESET_ARTIFACT_ROOT),
            PathBuf::from(CHANGESET_ARTIFACT_ROOT),
        )
    }

    /// Creates a store rooted at an injected artifact directory.
    ///
    /// `artifact_label_root` is returned in model-visible metadata instead of the absolute
    /// machine-local artifact root.
    ///
    /// # Errors
    ///
    /// Returns an error when `workspace_root` cannot be canonicalized.
    pub fn new_with_artifact_root(
        workspace_root: impl AsRef<Path>,
        artifact_root: impl AsRef<Path>,
        artifact_label_root: impl Into<PathBuf>,
    ) -> Result<Self> {
        let workspace_root = canonical_workspace_root(workspace_root.as_ref())?;
        Ok(Self {
            artifact_root: absolute_path_from(&workspace_root, artifact_root.as_ref()),
            artifact_label_root: artifact_label_root.into(),
            workspace_root,
            summary_limit_bytes: DEFAULT_CHANGESET_SUMMARY_LIMIT_BYTES,
        })
    }

    /// Overrides the bounded summary byte budget.
    pub fn with_summary_limit_bytes(mut self, summary_limit_bytes: usize) -> Self {
        self.summary_limit_bytes = summary_limit_bytes.max(1);
        self
    }

    /// Writes preview and reverse diffs using the stable change set artifact layout.
    ///
    /// # Errors
    ///
    /// Returns an error when the artifact path would leave the workspace or when files cannot be
    /// written.
    pub fn write_diff_artifacts(
        &self,
        change_set_id: ChangeSetId,
        preview_diff: &str,
        reverse_diff: &str,
    ) -> Result<ChangeSetArtifactRecord> {
        let paths = self.artifact_paths(&change_set_id)?;
        fs::create_dir_all(&paths.absolute_dir)
            .with_context(|| format!("failed to create {}", paths.absolute_dir.display()))?;
        fs::write(&paths.absolute_preview, preview_diff.as_bytes())
            .with_context(|| format!("failed to write {}", paths.absolute_preview.display()))?;
        fs::write(&paths.absolute_reverse, reverse_diff.as_bytes())
            .with_context(|| format!("failed to write {}", paths.absolute_reverse.display()))?;

        let preview = ChangeSetDiffArtifact {
            path: paths.relative_preview,
            sha256: sha256_hex(preview_diff.as_bytes()),
            bytes: preview_diff.len() as u64,
            line_count: preview_diff.lines().count() as u64,
            stats: ToolDiffStats::from_unified_diff(preview_diff),
        };
        let reverse = ChangeSetDiffArtifact {
            path: paths.relative_reverse,
            sha256: sha256_hex(reverse_diff.as_bytes()),
            bytes: reverse_diff.len() as u64,
            line_count: reverse_diff.lines().count() as u64,
            stats: ToolDiffStats::from_unified_diff(reverse_diff),
        };
        let limited = limit_text_head_tail(preview_diff, self.summary_limit_bytes);

        Ok(ChangeSetArtifactRecord {
            change_set_id,
            artifact_dir: paths.relative_dir,
            preview,
            reverse,
            summary: ChangeSetArtifactSummary {
                text: limited.content,
                truncated: limited.truncated,
                returned_bytes: limited.returned_bytes,
                omitted_bytes: limited.omitted_bytes,
                total_bytes: limited.total_bytes,
                total_lines: limited.total_lines,
                limit_bytes: self.summary_limit_bytes as u64,
            },
        })
    }

    /// Verifies that a recorded diff artifact still matches its hash.
    ///
    /// # Errors
    ///
    /// Returns an error when the recorded path is outside the workspace or cannot be read.
    pub fn verify_diff_artifact(&self, artifact: &ChangeSetDiffArtifact) -> Result<bool> {
        let path = self.stored_artifact_path(&artifact.path)?;
        let bytes =
            fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
        Ok(sha256_hex(&bytes) == artifact.sha256)
    }

    fn artifact_paths(&self, change_set_id: &ChangeSetId) -> Result<ChangeSetArtifactPaths> {
        let relative_dir = self
            .artifact_label_root
            .join(change_set_id.as_str())
            .to_string_lossy()
            .into_owned();
        let relative_preview = PathBuf::from(&relative_dir)
            .join(CHANGESET_PREVIEW_DIFF_FILE)
            .to_string_lossy()
            .into_owned();
        let relative_reverse = PathBuf::from(&relative_dir)
            .join(CHANGESET_REVERSE_DIFF_FILE)
            .to_string_lossy()
            .into_owned();
        let absolute_dir = self.artifact_root.join(change_set_id.as_str());
        Ok(ChangeSetArtifactPaths {
            absolute_preview: absolute_dir.join(CHANGESET_PREVIEW_DIFF_FILE),
            absolute_reverse: absolute_dir.join(CHANGESET_REVERSE_DIFF_FILE),
            absolute_dir,
            relative_dir,
            relative_preview,
            relative_reverse,
        })
    }

    fn stored_artifact_path(&self, relative_path: &str) -> Result<PathBuf> {
        let relative_path = Path::new(relative_path);
        if relative_path.is_absolute() {
            bail!(
                "change set artifact path must be relative: {}",
                relative_path.display()
            );
        }
        let suffix = relative_path
            .strip_prefix(&self.artifact_label_root)
            .with_context(|| {
                format!(
                    "change set artifact path has unknown label: {}",
                    relative_path.display()
                )
            })?;
        let lexical = lexically_normalize_path(&self.artifact_root.join(suffix))?;
        let resolved_prefix = resolve_existing_prefix(&lexical)?;
        if !resolved_prefix.starts_with(&self.artifact_root)
            && !resolved_prefix.starts_with(&self.workspace_root)
        {
            bail!(
                "change set artifact path is outside artifact root: {}",
                relative_path.display()
            );
        }
        Ok(lexical)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ApplyChangeSetArgs {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub risk: Option<ChangeSetRisk>,
    #[serde(default)]
    pub files: Vec<ApplyChangeSetFileArg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct ApplyChangeSetFileArg {
    pub path: String,
    pub action: ChangeSetFileAction,
    #[serde(default)]
    pub risk: Option<ChangeSetRisk>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub old_text: Option<String>,
    #[serde(default)]
    pub new_text: Option<String>,
    #[serde(default, alias = "expected_before_hash")]
    pub before_hash: Option<String>,
    #[serde(default, alias = "expected_mtime_ms")]
    pub before_mtime_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct ApplyChangeSetPlan {
    change_set: ChangeSet,
    files: Vec<PlannedChangeSetFile>,
    preview_diff: String,
    reverse_diff: String,
}

#[derive(Debug, Clone)]
struct PlannedChangeSetFile {
    path: String,
    absolute_path: PathBuf,
    action: ChangeSetFileAction,
    after_content: Option<String>,
    preview_diff: String,
    reverse_diff: String,
    validations: Vec<ChangeSetValidation>,
}

#[derive(Debug, Clone)]
struct ApplyChangeSetPlanError {
    message: String,
    result: ChangeSetResult,
}

impl std::fmt::Display for ApplyChangeSetPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApplyChangeSetPlanError {}

#[derive(Debug, Clone)]
struct PlannedChangeSetFailure {
    path: String,
    action: ChangeSetFileAction,
    validations: Vec<ChangeSetValidation>,
}

#[derive(Debug, Clone)]
struct ResolvedChangePath {
    normalized: String,
    absolute: PathBuf,
}

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
impl Tool for ApplyChangeSetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "apply_changeset".to_owned(),
            description: "Apply a validated multi-file workspace change set after approval."
                .to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "title": { "type": "string" },
                    "summary": { "type": "string" },
                    "risk": { "type": "string", "enum": ["low", "medium", "high"] },
                    "files": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string" },
                                "action": { "type": "string", "enum": ["create", "update", "delete"] },
                                "risk": { "type": "string", "enum": ["low", "medium", "high"] },
                                "content": { "type": "string" },
                                "old_text": { "type": "string" },
                                "new_text": { "type": "string" },
                                "before_hash": { "type": "string" },
                                "before_mtime_ms": { "type": "integer" }
                            },
                            "required": ["path", "action"]
                        }
                    }
                },
                "required": ["id", "files"]
            }),
            category: ToolCategory::File,
            access: ToolAccess::Write,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let args = parse_apply_changeset_args(args)?;
        if args.files.is_empty() {
            bail!("apply_changeset requires at least one file");
        }
        args.files
            .iter()
            .map(|file| tool_path_subject(&ctx.workspace_root, &file.path))
            .collect()
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let workspace_root = ctx.workspace_root.clone();
        let plan = match run_blocking_io("apply_changeset_plan", move || {
            build_apply_changeset_plan(&workspace_root, &args)
        })
        .await?
        {
            Ok(plan) => plan,
            Err(error) => {
                let details = apply_changeset_details(None, &error.result, None);
                let mut result = ToolResult::error(
                    call_id,
                    self.spec().name,
                    ToolErrorKind::InvalidInput,
                    error.message,
                )
                .with_error_details(false, details.clone());
                result.metadata.details = details;
                return Ok(result);
            }
        };

        let workspace_root = ctx.workspace_root.clone();
        let artifact_root = self.artifact_root.clone();
        let artifact_label_root = self.artifact_label_root.clone();
        let mutation_recorder = ctx.mutation_recorder.clone();
        Ok(run_blocking_io("apply_changeset", move || {
            apply_changeset_plan(
                &workspace_root,
                &artifact_root,
                artifact_label_root,
                call_id,
                mutation_recorder,
                plan,
            )
        })
        .await?)
    }

    async fn preview(&self, ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        let workspace_root = ctx.workspace_root.clone();
        let plan = run_blocking_io("apply_changeset_preview", move || {
            build_apply_changeset_plan(&workspace_root, &args)
        })
        .await??;

        Ok(Some(ToolPreview {
            title: plan.change_set.title.clone(),
            summary: format!(
                "{} files, risk={}",
                plan.change_set.files.len(),
                plan.change_set.risk.as_str()
            ),
            body: plan.preview_diff,
            changed_files: plan
                .change_set
                .files
                .iter()
                .map(|file| file.path.clone())
                .collect(),
            file_diffs: plan
                .files
                .iter()
                .map(|file| ToolPreviewFile {
                    path: file.path.clone(),
                    diff: file.preview_diff.clone(),
                })
                .collect(),
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
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let command = required_string(args, "command")?;
        let mut subjects = vec![ToolSubject::command(
            command.to_owned(),
            command_permission_subject(command),
        )];
        subjects.extend(bash_path_subjects(&ctx.workspace_root, command)?);
        Ok(subjects)
    }

    fn permission_access(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolAccess> {
        let command = required_string(args, "command")?;
        if bash_command_is_safe_readonly(command) {
            Ok(ToolAccess::Read)
        } else {
            Ok(ToolAccess::Execute)
        }
    }

    fn permission_operation(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let command = required_string(args, "command")?;
        Ok(shell_command_permission_operation(command))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let command = required_string(&args, "command")?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(ctx.timeout_secs);
        let scratch_root = absolute_path_from(&ctx.workspace_root, &self.scratch_root);
        tokio::fs::create_dir_all(&scratch_root)
            .await
            .with_context(|| format!("failed to create {}", self.scratch_label))?;
        let request =
            bash_execution_request(command, &ctx.workspace_root, &scratch_root, timeout_secs);
        let receipt = self.backend.execute(request).await?;
        bash_tool_result_from_execution_receipt(call_id, self.spec().name, receipt)
    }
}

fn bash_execution_request(
    command: &str,
    workspace_root: &Path,
    scratch_root: &Path,
    timeout_secs: u64,
) -> ExecutionRequest {
    ExecutionRequest {
        program: "sh".to_owned(),
        args: vec!["-lc".to_owned(), command.to_owned()],
        cwd: workspace_root.to_path_buf(),
        env: BTreeMap::from([(
            SIGIL_SCRATCH_DIR_ENV.to_owned(),
            scratch_root.to_string_lossy().into_owned(),
        )]),
        timeout_secs,
    }
}

fn bash_tool_result_from_execution_receipt(
    call_id: String,
    tool_name: String,
    receipt: ExecutionReceipt,
) -> Result<ToolResult> {
    if receipt.timed_out {
        return Ok(ToolResult::error(
            call_id,
            tool_name,
            ToolErrorKind::Timeout,
            "bash command timed out",
        ));
    }
    let stdout = String::from_utf8_lossy(&receipt.stdout);
    let stderr = String::from_utf8_lossy(&receipt.stderr);
    let limit_bytes = DEFAULT_TEXT_LIMIT_BYTES.min(HARD_TEXT_LIMIT_BYTES);
    let limited_stdout = limit_text_head_tail(&stdout, limit_bytes);
    let limited_stderr = limit_text_head_tail(&stderr, limit_bytes);
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
    let metadata = ToolResultMeta {
        exit_code: receipt.exit_code,
        stdout_bytes: Some(receipt.stdout.len() as u64),
        stderr_bytes: Some(receipt.stderr.len() as u64),
        truncated: limited_stdout.truncated || limited_stderr.truncated,
        omitted_bytes: Some(limited_stdout.omitted_bytes + limited_stderr.omitted_bytes),
        limit_bytes: Some(limit_bytes as u64),
        returned_bytes: Some(limited_stdout.returned_bytes + limited_stderr.returned_bytes),
        total_bytes: Some(receipt.stdout.len() as u64 + receipt.stderr.len() as u64),
        returned_lines: Some(limited_stdout.returned_lines + limited_stderr.returned_lines),
        total_lines: Some(limited_stdout.total_lines + limited_stderr.total_lines),
        ..ToolResultMeta::default()
    };
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

#[async_trait]
impl Tool for TerminalStartTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "terminal_start".to_owned(),
            description: format!(
                "Start a background terminal task from the workspace, optionally with PTY support. Use ${SIGIL_SCRATCH_DIR_ENV} for temporary shell files (shown as {}); OS temp directories are outside the workspace and require permission.external_directory.",
                self.scratch_label
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string" },
                    "command": { "type": "string" },
                    "cwd": { "type": "string" },
                    "shell": { "type": "string" },
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
        let mut subjects = vec![ToolSubject::command(
            command.to_owned(),
            command_permission_subject(command),
        )];
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

    fn permission_operation(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolOperation> {
        let command = required_string(args, "command")?;
        Ok(shell_command_permission_operation(command))
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
        Ok(terminal_entry_result(
            call_id,
            self.spec().name,
            "started",
            entry,
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

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing string field {key}"))
}

fn optional_string<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn optional_usize(args: &Value, key: &str) -> Result<Option<usize>> {
    args.get(key)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| anyhow!("{key} must be a positive integer"))
                .and_then(|value| {
                    usize::try_from(value)
                        .map_err(|_| anyhow!("{key} is too large for this platform"))
                })
        })
        .transpose()
}

#[derive(Debug, Clone)]
struct TerminalStartArgs {
    task_id: Option<TerminalTaskId>,
    command: String,
    cwd: Option<PathBuf>,
    shell: Option<String>,
    pty: bool,
    pty_size: Option<TerminalPtySize>,
}

fn parse_terminal_start_args(args: &Value) -> Result<TerminalStartArgs> {
    let task_id = optional_string(args, "task_id")
        .map(|task_id| TerminalTaskId::new(task_id.to_owned()))
        .transpose()?;
    let command = required_string(args, "command")?.to_owned();
    let cwd = optional_string(args, "cwd").map(PathBuf::from);
    let shell = optional_string(args, "shell").map(str::to_owned);
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
        pty,
        pty_size,
    })
}

fn required_terminal_task_id(args: &Value) -> Result<TerminalTaskId> {
    TerminalTaskId::new(required_string(args, "task_id")?.to_owned())
}

fn required_terminal_pty_size(args: &Value) -> Result<TerminalPtySize> {
    TerminalPtySize::new(required_u16(args, "rows")?, required_u16(args, "cols")?)
}

fn required_u16(args: &Value, key: &str) -> Result<u16> {
    let value = args
        .get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("missing positive integer field {key}"))?;
    u16::try_from(value).map_err(|_| anyhow!("{key} is too large for a terminal dimension"))
}

fn terminal_read_limit(args: &Value) -> Result<usize> {
    Ok(optional_usize(args, "limit_bytes")?
        .unwrap_or(DEFAULT_TERMINAL_READ_LIMIT_BYTES)
        .clamp(1, HARD_TERMINAL_READ_LIMIT_BYTES))
}

fn terminal_task_subject(task_id: &TerminalTaskId) -> ToolSubject {
    let value = format!("terminal_task:{}", task_id.as_str());
    ToolSubject::command(value.clone(), value)
}

fn terminal_input_subject(input_bytes: usize) -> ToolSubject {
    ToolSubject::command(
        format!("terminal_input bytes={input_bytes}"),
        format!("terminal_input_bytes:{input_bytes}"),
    )
}

fn validate_terminal_input_len(input: &str) -> Result<()> {
    if input.len() > MAX_TERMINAL_INPUT_BYTES {
        bail!(
            "terminal_input input exceeds maximum of {} bytes",
            MAX_TERMINAL_INPUT_BYTES
        );
    }
    Ok(())
}

fn terminal_command_path_subjects(
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

fn terminal_entry_result(
    call_id: String,
    tool_name: String,
    action: &'static str,
    entry: TerminalTaskEntry,
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
            details: terminal_entry_details(&entry),
            ..ToolResultMeta::default()
        },
    )
}

fn terminal_entry_details(entry: &TerminalTaskEntry) -> Value {
    json!({
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
    })
}

fn terminal_input_result(
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

fn terminal_resize_result(
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

fn is_terminal_backend_unsupported(error: &anyhow::Error) -> bool {
    let message = error.to_string();
    message.contains("backend does not support input")
        || message.contains("backend does not support resize")
}

fn terminal_read_details(read: &TerminalReadResult, limit_bytes: usize) -> Value {
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
        object.insert("terminal_task".to_owned(), terminal_entry_details(entry));
    }
    details
}

fn parse_apply_changeset_args(args: &Value) -> Result<ApplyChangeSetArgs> {
    serde_json::from_value(args.clone()).context("invalid apply_changeset arguments")
}

fn build_apply_changeset_plan(
    workspace_root: &Path,
    args: &Value,
) -> Result<std::result::Result<ApplyChangeSetPlan, ApplyChangeSetPlanError>> {
    let args = parse_apply_changeset_args(args)?;
    let change_set_id = ChangeSetId::new(args.id)?;
    if args.files.is_empty() {
        bail!("apply_changeset requires at least one file");
    }

    let risk = args.risk.unwrap_or(ChangeSetRisk::Medium);
    let mut planned_files = Vec::new();
    let mut change_set_files = Vec::new();
    let mut failures = Vec::new();
    let mut seen_paths = BTreeSet::new();

    for file in args.files {
        if !seen_paths.insert(file.path.clone()) {
            failures.push(PlannedChangeSetFailure {
                path: file.path.clone(),
                action: file.action,
                validations: vec![validation_failed(
                    ChangeSetValidationKind::Path,
                    "duplicate_path: change set contains the same path more than once",
                )],
            });
            continue;
        }

        match plan_changeset_file(workspace_root, file, risk) {
            Ok((planned, change_set_file)) => {
                planned_files.push(planned);
                change_set_files.push(change_set_file);
            }
            Err(failure) => failures.push(failure),
        }
    }

    if !failures.is_empty() {
        return Ok(Err(validation_plan_error(change_set_id, failures)));
    }

    let title = args
        .title
        .unwrap_or_else(|| format!("Apply change set {}", change_set_id.as_str()));
    let summary = args
        .summary
        .unwrap_or_else(|| format!("Apply {} file changes", change_set_files.len()));
    let preview_diff = planned_files
        .iter()
        .map(|file| file.preview_diff.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let reverse_diff = planned_files
        .iter()
        .map(|file| file.reverse_diff.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    Ok(Ok(ApplyChangeSetPlan {
        change_set: ChangeSet {
            id: change_set_id,
            title,
            summary,
            risk,
            files: change_set_files,
            validations: Vec::new(),
        },
        files: planned_files,
        preview_diff,
        reverse_diff,
    }))
}

fn plan_changeset_file(
    workspace_root: &Path,
    file: ApplyChangeSetFileArg,
    default_risk: ChangeSetRisk,
) -> std::result::Result<(PlannedChangeSetFile, ChangeSetFile), PlannedChangeSetFailure> {
    let path = file.path.clone();
    let action = file.action;
    let risk = file.risk.unwrap_or(default_risk);
    let mut validations = Vec::new();

    enum SupportedAction {
        Create,
        Update,
        Delete,
    }
    let supported_action = match action {
        ChangeSetFileAction::Create => SupportedAction::Create,
        ChangeSetFileAction::Update => SupportedAction::Update,
        ChangeSetFileAction::Delete => SupportedAction::Delete,
        ChangeSetFileAction::Rename => {
            return Err(file_failure(
                path,
                action,
                vec![validation_failed(
                    ChangeSetValidationKind::Custom,
                    "unsupported_action: apply_changeset does not support rename",
                )],
            ));
        }
    };

    let resolved = resolve_workspace_change_path(workspace_root, &path).map_err(|error| {
        file_failure(
            path.clone(),
            action,
            vec![validation_failed(
                ChangeSetValidationKind::Path,
                format!("path_outside_workspace: {error}"),
            )],
        )
    })?;
    validations.push(validation_passed(ChangeSetValidationKind::Path, None));

    let before_snapshot = read_text_snapshot(&resolved.absolute, &path).map_err(|issue| {
        file_failure(
            resolved.normalized.clone(),
            action,
            vec![validation_failed(issue.kind, issue.message)],
        )
    })?;

    validate_expected_hash(&file, before_snapshot.as_ref(), &mut validations).map_err(|issue| {
        file_failure(
            resolved.normalized.clone(),
            action,
            append_failed_validation(validations.clone(), issue),
        )
    })?;
    validate_expected_mtime(&file, before_snapshot.as_ref(), &mut validations).map_err(
        |issue| {
            file_failure(
                resolved.normalized.clone(),
                action,
                append_failed_validation(validations.clone(), issue),
            )
        },
    )?;

    let before_content = before_snapshot
        .as_ref()
        .map(|snapshot| snapshot.content.as_str())
        .unwrap_or_default();
    let after_content = match supported_action {
        SupportedAction::Create => {
            if before_snapshot.is_some() {
                return Err(file_failure(
                    resolved.normalized,
                    action,
                    vec![validation_failed(
                        ChangeSetValidationKind::Path,
                        "target_exists: create target already exists",
                    )],
                ));
            }
            let content = file.content.ok_or_else(|| {
                file_failure(
                    path.clone(),
                    action,
                    vec![validation_failed(
                        ChangeSetValidationKind::Custom,
                        "missing_content: create requires content",
                    )],
                )
            })?;
            validate_text_content(&content).map_err(|issue| {
                file_failure(
                    path.clone(),
                    action,
                    append_failed_validation(validations.clone(), issue),
                )
            })?;
            Some(content)
        }
        SupportedAction::Update => {
            let before = before_snapshot.as_ref().ok_or_else(|| {
                file_failure(
                    resolved.normalized.clone(),
                    action,
                    vec![validation_failed(
                        ChangeSetValidationKind::Path,
                        "missing_file: update target does not exist",
                    )],
                )
            })?;
            let full_replacement = file.content.is_some();
            let snippet_replacement = file.old_text.is_some() || file.new_text.is_some();
            if full_replacement && snippet_replacement {
                return Err(file_failure(
                    resolved.normalized,
                    action,
                    vec![validation_failed(
                        ChangeSetValidationKind::Custom,
                        "ambiguous_update: provide either content or old_text/new_text",
                    )],
                ));
            }
            if let Some(content) = file.content {
                validate_text_content(&content).map_err(|issue| {
                    file_failure(
                        path.clone(),
                        action,
                        append_failed_validation(validations.clone(), issue),
                    )
                })?;
                Some(content)
            } else {
                let old_text = file.old_text.ok_or_else(|| {
                    file_failure(
                        resolved.normalized.clone(),
                        action,
                        vec![validation_failed(
                            ChangeSetValidationKind::Snippet,
                            "missing_snippet: update requires old_text with new_text",
                        )],
                    )
                })?;
                let new_text = file.new_text.ok_or_else(|| {
                    file_failure(
                        resolved.normalized.clone(),
                        action,
                        vec![validation_failed(
                            ChangeSetValidationKind::Snippet,
                            "missing_snippet: update requires new_text with old_text",
                        )],
                    )
                })?;
                validate_text_content(&new_text).map_err(|issue| {
                    file_failure(
                        path.clone(),
                        action,
                        append_failed_validation(validations.clone(), issue),
                    )
                })?;
                let occurrences = before.content.matches(&old_text).count();
                if occurrences == 0 {
                    return Err(file_failure(
                        resolved.normalized,
                        action,
                        append_failed_validation(
                            validations,
                            ValidationIssue::new(
                                ChangeSetValidationKind::Snippet,
                                "snippet_missing: old_text was not found",
                            ),
                        ),
                    ));
                }
                if occurrences > 1 {
                    return Err(file_failure(
                        resolved.normalized,
                        action,
                        append_failed_validation(
                            validations,
                            ValidationIssue::new(
                                ChangeSetValidationKind::Snippet,
                                "snippet_ambiguous: old_text matched more than once",
                            ),
                        ),
                    ));
                }
                validations.push(validation_passed(ChangeSetValidationKind::Snippet, None));
                Some(before.content.replacen(&old_text, &new_text, 1))
            }
        }
        SupportedAction::Delete => {
            if file.content.is_some() || file.old_text.is_some() || file.new_text.is_some() {
                return Err(file_failure(
                    resolved.normalized,
                    action,
                    vec![validation_failed(
                        ChangeSetValidationKind::Custom,
                        "invalid_delete_payload: delete does not accept content or snippets",
                    )],
                ));
            }
            before_snapshot.as_ref().ok_or_else(|| {
                file_failure(
                    resolved.normalized.clone(),
                    action,
                    vec![validation_failed(
                        ChangeSetValidationKind::Path,
                        "missing_file: delete target does not exist",
                    )],
                )
            })?;
            None
        }
    };

    let after_content_ref = after_content.as_deref().unwrap_or_default();
    let preview_diff = render_unified_diff(
        before_content,
        after_content_ref,
        &format!("current/{}", resolved.normalized),
        &format!("proposed/{}", resolved.normalized),
    );
    let reverse_diff = render_unified_diff(
        after_content_ref,
        before_content,
        &format!("applied/{}", resolved.normalized),
        &format!("rollback/{}", resolved.normalized),
    );
    let stats = ToolDiffStats::from_unified_diff(&preview_diff);

    let change_set_file = ChangeSetFile {
        path: resolved.normalized.clone(),
        previous_path: None,
        action,
        risk,
        before_hash: before_snapshot
            .as_ref()
            .map(|snapshot| snapshot.hash.clone()),
        after_hash: after_content
            .as_ref()
            .map(|content| sha256_hex(content.as_bytes())),
        diff_hash: Some(sha256_hex(preview_diff.as_bytes())),
        additions: stats.added as u32,
        deletions: stats.removed as u32,
        validations: validations.clone(),
    };
    Ok((
        PlannedChangeSetFile {
            path: resolved.normalized,
            absolute_path: resolved.absolute,
            action,
            after_content,
            preview_diff,
            reverse_diff,
            validations,
        },
        change_set_file,
    ))
}

fn apply_changeset_plan(
    workspace_root: &Path,
    artifact_root: &Path,
    artifact_label_root: PathBuf,
    call_id: String,
    mutation_recorder: Option<MutationEventRecorder>,
    plan: ApplyChangeSetPlan,
) -> Result<ToolResult> {
    let mut file_results = Vec::new();
    let mut changed_files = Vec::new();
    let mut applied_preview_diffs = Vec::new();
    let mut applied_reverse_diffs = Vec::new();
    let mut committed_operations = Vec::new();
    let mut failed_operations = Vec::new();
    let mut failed = false;
    let batch_id: MutationBatchId = format!("changeset:{}", plan.change_set.id.as_str());
    if let Some(recorder) = mutation_recorder.as_ref() {
        let expected_subjects = plan
            .files
            .iter()
            .map(|file| MutationSubject::File {
                path: PathBuf::from(&file.path),
                file_type: FileType::File,
            })
            .collect::<Vec<_>>();
        recorder.append_batch_started(
            &batch_id,
            &format!("apply_changeset:{call_id}"),
            &expected_subjects,
        )?;
    }

    for file in &plan.files {
        if failed {
            file_results.push(ChangeSetFileResult {
                path: file.path.clone(),
                action: file.action,
                status: ChangeSetFileResultStatus::Skipped,
                message: Some("skipped after prior apply failure".to_owned()),
                validations: file.validations.clone(),
            });
            continue;
        }

        match apply_planned_changeset_file(
            file,
            mutation_recorder.as_ref(),
            workspace_root,
            &call_id,
            Some(batch_id.clone()),
        ) {
            Ok(mutation) => {
                if let Some(mutation) = mutation {
                    committed_operations.push(mutation.operation_id);
                }
                changed_files.push(file.path.clone());
                applied_preview_diffs.push(file.preview_diff.clone());
                applied_reverse_diffs.push(file.reverse_diff.clone());
                file_results.push(ChangeSetFileResult {
                    path: file.path.clone(),
                    action: file.action,
                    status: ChangeSetFileResultStatus::Applied,
                    message: None,
                    validations: file.validations.clone(),
                });
            }
            Err(error) => {
                failed = true;
                failed_operations.push(format!(
                    "apply_changeset:{}:{}",
                    plan.change_set.id.as_str(),
                    file.path
                ));
                file_results.push(ChangeSetFileResult {
                    path: file.path.clone(),
                    action: file.action,
                    status: ChangeSetFileResultStatus::Failed,
                    message: Some(error.to_string()),
                    validations: append_failed_validation(
                        file.validations.clone(),
                        ValidationIssue::new(
                            ChangeSetValidationKind::Custom,
                            format!("apply_io: {error}"),
                        ),
                    ),
                });
            }
        }
    }

    let status = if failed {
        if changed_files.is_empty() {
            ChangeSetResultStatus::Failed
        } else {
            ChangeSetResultStatus::PartiallyApplied
        }
    } else {
        ChangeSetResultStatus::Applied
    };
    if let Some(recorder) = mutation_recorder.as_ref() {
        let batch_status = match status {
            ChangeSetResultStatus::Applied => MutationBatchStatus::Applied,
            ChangeSetResultStatus::PartiallyApplied => MutationBatchStatus::PartiallyApplied,
            ChangeSetResultStatus::Failed | ChangeSetResultStatus::Cancelled => {
                MutationBatchStatus::Failed
            }
        };
        recorder.append_batch_finished(
            &batch_id,
            batch_status,
            &committed_operations,
            &failed_operations,
        )?;
    }
    let mut apply_result = ChangeSetResult {
        id: plan.change_set.id.clone(),
        status,
        file_results,
        message: None,
    };

    let artifact_record = if changed_files.is_empty() {
        None
    } else {
        let preview_diff = if status == ChangeSetResultStatus::Applied {
            plan.preview_diff.clone()
        } else {
            applied_preview_diffs.join("\n")
        };
        let reverse_diff = if status == ChangeSetResultStatus::Applied {
            plan.reverse_diff.clone()
        } else {
            applied_reverse_diffs.join("\n")
        };
        match ChangeSetArtifactStore::new_with_artifact_root(
            workspace_root,
            artifact_root,
            artifact_label_root,
        )?
        .write_diff_artifacts(plan.change_set.id.clone(), &preview_diff, &reverse_diff)
        {
            Ok(record) => Some(record),
            Err(error) => {
                apply_result.status = ChangeSetResultStatus::PartiallyApplied;
                apply_result.message = Some(format!("artifact_write_failed: {error}"));
                return Ok(apply_changeset_error_result(
                    call_id,
                    plan,
                    apply_result,
                    None,
                    changed_files,
                    ToolErrorKind::Io,
                    "change set applied but artifact write failed",
                ));
            }
        }
    };

    if failed {
        apply_result.message = Some("partial apply failure".to_owned());
        return Ok(apply_changeset_error_result(
            call_id,
            plan,
            apply_result,
            artifact_record,
            changed_files,
            ToolErrorKind::Io,
            "change set partially applied",
        ));
    }

    let details = apply_changeset_details(
        Some(&plan.change_set),
        &apply_result,
        artifact_record.as_ref(),
    );
    Ok(ToolResult::ok(
        call_id,
        "apply_changeset",
        format!(
            "applied change set {} ({} files)",
            plan.change_set.id.as_str(),
            changed_files.len()
        ),
        ToolResultMeta {
            changed_files,
            details,
            ..ToolResultMeta::default()
        },
    ))
}

fn apply_planned_changeset_file(
    file: &PlannedChangeSetFile,
    mutation_recorder: Option<&MutationEventRecorder>,
    workspace_root: &Path,
    call_id: &str,
    batch_id: Option<MutationBatchId>,
) -> Result<Option<CommittedFileMutation>> {
    match file.action {
        ChangeSetFileAction::Create | ChangeSetFileAction::Update => {
            let content = file
                .after_content
                .as_ref()
                .ok_or_else(|| anyhow!("missing proposed content for {}", file.path))?;
            write_file_with_mutation_in_batch(
                mutation_recorder,
                workspace_root,
                call_id,
                batch_id,
                PathBuf::from(&file.path),
                file.absolute_path.clone(),
                content.as_bytes(),
            )
        }
        ChangeSetFileAction::Delete => delete_file_with_mutation_in_batch(
            mutation_recorder,
            workspace_root,
            call_id,
            batch_id,
            PathBuf::from(&file.path),
            file.absolute_path.clone(),
        ),
        ChangeSetFileAction::Rename => bail!("rename is not supported by apply_changeset"),
    }
}

fn apply_changeset_error_result(
    call_id: String,
    plan: ApplyChangeSetPlan,
    apply_result: ChangeSetResult,
    artifact_record: Option<ChangeSetArtifactRecord>,
    changed_files: Vec<String>,
    kind: ToolErrorKind,
    message: &str,
) -> ToolResult {
    let details = apply_changeset_details(
        Some(&plan.change_set),
        &apply_result,
        artifact_record.as_ref(),
    );
    let mut result = ToolResult::error(call_id, "apply_changeset", kind, message)
        .with_error_details(false, details.clone());
    result.metadata = ToolResultMeta {
        changed_files,
        details,
        ..ToolResultMeta::default()
    };
    result
}

fn apply_changeset_details(
    change_set: Option<&ChangeSet>,
    apply_result: &ChangeSetResult,
    artifact_record: Option<&ChangeSetArtifactRecord>,
) -> Value {
    let mut details = serde_json::Map::new();
    if let Some(change_set) = change_set {
        details.insert("change_set".to_owned(), json!(change_set));
    }
    details.insert("apply_result".to_owned(), json!(apply_result));
    if let Some(record) = artifact_record {
        details.insert(
            "artifacts".to_owned(),
            json!({
                "artifact_dir": record.artifact_dir,
                "preview": record.preview,
                "reverse": record.reverse,
                "summary": {
                    "truncated": record.summary.truncated,
                    "returned_bytes": record.summary.returned_bytes,
                    "omitted_bytes": record.summary.omitted_bytes,
                    "total_bytes": record.summary.total_bytes,
                    "total_lines": record.summary.total_lines,
                    "limit_bytes": record.summary.limit_bytes
                }
            }),
        );
    }
    Value::Object(details)
}

fn validation_plan_error(
    change_set_id: ChangeSetId,
    failures: Vec<PlannedChangeSetFailure>,
) -> ApplyChangeSetPlanError {
    let file_results = failures
        .into_iter()
        .map(|failure| {
            let message = failure
                .validations
                .iter()
                .find(|validation| validation.status == ChangeSetValidationStatus::Failed)
                .and_then(|validation| validation.message.clone());
            ChangeSetFileResult {
                path: failure.path,
                action: failure.action,
                status: ChangeSetFileResultStatus::Failed,
                message,
                validations: failure.validations,
            }
        })
        .collect();
    ApplyChangeSetPlanError {
        message: "change set validation failed".to_owned(),
        result: ChangeSetResult {
            id: change_set_id,
            status: ChangeSetResultStatus::Failed,
            file_results,
            message: Some("change set validation failed".to_owned()),
        },
    }
}

fn file_failure(
    path: String,
    action: ChangeSetFileAction,
    validations: Vec<ChangeSetValidation>,
) -> PlannedChangeSetFailure {
    PlannedChangeSetFailure {
        path,
        action,
        validations,
    }
}

#[derive(Debug, Clone)]
struct TextFileSnapshot {
    content: String,
    hash: String,
    mtime_ms: Option<u64>,
}

#[derive(Debug, Clone)]
struct ValidationIssue {
    kind: ChangeSetValidationKind,
    message: String,
}

impl ValidationIssue {
    fn new(kind: ChangeSetValidationKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

fn read_text_snapshot(
    path: &Path,
    display_path: &str,
) -> std::result::Result<Option<TextFileSnapshot>, ValidationIssue> {
    let symlink_metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(ValidationIssue::new(
                ChangeSetValidationKind::Path,
                format!("path_error: failed to inspect {display_path}: {error}"),
            ));
        }
    };
    if symlink_metadata.file_type().is_symlink() {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Symlink,
            "symlink_escape: symlink paths are not supported",
        ));
    }
    let metadata = fs::metadata(path).map_err(|error| {
        ValidationIssue::new(
            ChangeSetValidationKind::Path,
            format!("path_error: failed to inspect {display_path}: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Path,
            "not_regular_file: target is not a regular file",
        ));
    }
    let bytes = fs::read(path).map_err(|error| {
        ValidationIssue::new(
            ChangeSetValidationKind::Path,
            format!("path_error: failed to read {display_path}: {error}"),
        )
    })?;
    if bytes.contains(&0) {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Binary,
            "binary_file: target contains NUL bytes",
        ));
    }
    let content = String::from_utf8(bytes).map_err(|error| {
        ValidationIssue::new(
            ChangeSetValidationKind::Binary,
            format!("binary_file: target is not valid UTF-8: {error}"),
        )
    })?;
    Ok(Some(TextFileSnapshot {
        hash: sha256_hex(content.as_bytes()),
        content,
        mtime_ms: metadata_mtime_ms(&metadata),
    }))
}

fn validate_text_content(content: &str) -> std::result::Result<(), ValidationIssue> {
    if content.contains('\0') {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Binary,
            "binary_file: proposed content contains NUL bytes",
        ));
    }
    Ok(())
}

fn validate_expected_hash(
    file: &ApplyChangeSetFileArg,
    snapshot: Option<&TextFileSnapshot>,
    validations: &mut Vec<ChangeSetValidation>,
) -> std::result::Result<(), ValidationIssue> {
    let Some(expected) = file.before_hash.as_ref() else {
        return Ok(());
    };
    let Some(snapshot) = snapshot else {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Hash,
            "hash_mismatch: expected existing file hash but target is missing",
        ));
    };
    if expected != &snapshot.hash {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Hash,
            format!(
                "hash_mismatch: expected {expected}, observed {}",
                snapshot.hash
            ),
        ));
    }
    validations.push(validation_passed(ChangeSetValidationKind::Hash, None));
    Ok(())
}

fn validate_expected_mtime(
    file: &ApplyChangeSetFileArg,
    snapshot: Option<&TextFileSnapshot>,
    validations: &mut Vec<ChangeSetValidation>,
) -> std::result::Result<(), ValidationIssue> {
    let Some(expected) = file.before_mtime_ms else {
        return Ok(());
    };
    let Some(observed) = snapshot.and_then(|snapshot| snapshot.mtime_ms) else {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Mtime,
            "mtime_changed: current mtime is unavailable",
        ));
    };
    if expected != observed {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Mtime,
            format!("mtime_changed: expected {expected}, observed {observed}"),
        ));
    }
    validations.push(validation_passed(ChangeSetValidationKind::Mtime, None));
    Ok(())
}

fn append_failed_validation(
    mut validations: Vec<ChangeSetValidation>,
    issue: ValidationIssue,
) -> Vec<ChangeSetValidation> {
    validations.push(validation_failed(issue.kind, issue.message));
    validations
}

fn validation_passed(
    kind: ChangeSetValidationKind,
    message: Option<String>,
) -> ChangeSetValidation {
    ChangeSetValidation {
        kind,
        status: ChangeSetValidationStatus::Passed,
        message,
    }
}

fn validation_failed(
    kind: ChangeSetValidationKind,
    message: impl Into<String>,
) -> ChangeSetValidation {
    ChangeSetValidation {
        kind,
        status: ChangeSetValidationStatus::Failed,
        message: Some(message.into()),
    }
}

fn resolve_workspace_change_path(
    workspace_root: &Path,
    requested: &str,
) -> Result<ResolvedChangePath> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let requested_path = Path::new(requested);
    let lexical_target = if requested_path.is_absolute() {
        lexically_normalize_path(requested_path)?
    } else {
        lexically_normalize_path(&workspace_root.join(requested_path))?
    };
    let resolved_prefix = resolve_existing_prefix(&lexical_target)?;
    if !resolved_prefix.starts_with(&workspace_root) {
        bail!("path is outside workspace: {requested}");
    }
    Ok(ResolvedChangePath {
        normalized: relativize(&workspace_root, &lexical_target)?,
        absolute: lexical_target,
    })
}

fn metadata_mtime_ms(metadata: &fs::Metadata) -> Option<u64> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(duration.as_millis()).ok()
}

#[derive(Debug, Clone, Default)]
struct TextLimitResult {
    content: String,
    returned_bytes: u64,
    returned_lines: u64,
    total_bytes: u64,
    total_lines: u64,
    truncated: bool,
    omitted_bytes: u64,
}

fn limit_text_head(input: &str, max_bytes: usize, max_lines: usize) -> TextLimitResult {
    let mut output = String::new();
    let mut returned_lines = 0usize;
    let mut returned_bytes = 0usize;
    let total_lines = input.lines().count();
    let total_bytes = input.len();
    let mut truncated = false;

    for line in input.lines() {
        if returned_lines >= max_lines {
            truncated = true;
            break;
        }
        let line = truncate_line_for_model(line);
        let separator_bytes = usize::from(!output.is_empty());
        if returned_bytes + separator_bytes + line.len() > max_bytes {
            truncated = true;
            break;
        }
        if !output.is_empty() {
            output.push('\n');
            returned_bytes += 1;
        }
        returned_bytes += line.len();
        returned_lines += 1;
        output.push_str(&line);
    }

    if truncated {
        append_truncation_notice(&mut output);
    }

    TextLimitResult {
        content: output,
        returned_bytes: returned_bytes as u64,
        returned_lines: returned_lines as u64,
        total_bytes: total_bytes as u64,
        total_lines: total_lines as u64,
        truncated,
        omitted_bytes: total_bytes.saturating_sub(returned_bytes) as u64,
    }
}

fn limit_text_head_tail(input: &str, max_bytes: usize) -> TextLimitResult {
    if input.len() <= max_bytes {
        return TextLimitResult {
            content: input.to_owned(),
            returned_bytes: input.len() as u64,
            returned_lines: input.lines().count() as u64,
            total_bytes: input.len() as u64,
            total_lines: input.lines().count() as u64,
            truncated: false,
            omitted_bytes: 0,
        };
    }

    let head_budget = max_bytes / 2;
    let tail_budget = max_bytes.saturating_sub(head_budget);
    let head_end = floor_char_boundary(input, head_budget);
    let tail_start = ceil_char_boundary(input, input.len().saturating_sub(tail_budget));
    let omitted_bytes = tail_start.saturating_sub(head_end);
    let mut content = String::new();
    content.push_str(&input[..head_end]);
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&format!(
        "[sigil: output truncated, omitted {omitted_bytes} bytes]\n"
    ));
    content.push_str(&input[tail_start..]);
    TextLimitResult {
        returned_bytes: (input.len() - omitted_bytes) as u64,
        returned_lines: content.lines().count() as u64,
        total_bytes: input.len() as u64,
        total_lines: input.lines().count() as u64,
        truncated: true,
        omitted_bytes: omitted_bytes as u64,
        content,
    }
}

fn truncate_line_for_model(line: &str) -> String {
    if line.chars().count() <= MAX_MODEL_LINE_CHARS {
        line.to_owned()
    } else {
        let mut truncated = line.chars().take(MAX_MODEL_LINE_CHARS).collect::<String>();
        truncated.push_str("[sigil: line truncated]");
        truncated
    }
}

fn append_truncation_notice(output: &mut String) {
    if !output.is_empty() {
        output.push('\n');
    }
    output.push_str(
        "[sigil: output truncated; use offset/limit or a narrower path/pattern to continue]",
    );
}

fn floor_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while !value.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(value: &str, index: usize) -> usize {
    let mut index = index.min(value.len());
    while index < value.len() && !value.is_char_boundary(index) {
        index += 1;
    }
    index
}

#[derive(Debug, Clone)]
struct ResolvedToolPath {
    original: String,
    normalized: String,
    canonical: PathBuf,
    scope: ToolSubjectScope,
}

#[derive(Debug, Clone)]
struct DeleteFileTarget {
    path: PathBuf,
    display_path: String,
}

fn resolve_workspace_path(workspace_root: &Path, requested: &str) -> Result<PathBuf> {
    Ok(resolve_tool_path(workspace_root, requested)?.canonical)
}

fn tool_path_subject(workspace_root: &Path, requested: &str) -> Result<ToolSubject> {
    let resolved = resolve_tool_path(workspace_root, requested)?;
    Ok(ToolSubject::path_with_scope(
        resolved.original,
        resolved.normalized,
        Some(resolved.canonical),
        resolved.scope,
    ))
}

fn resolve_tool_path(workspace_root: &Path, requested: &str) -> Result<ResolvedToolPath> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    resolve_tool_path_from_base(&workspace_root, &workspace_root, requested)
}

fn resolve_delete_file_target(workspace_root: &Path, requested: &str) -> Result<DeleteFileTarget> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    let resolved = resolve_tool_path_from_base(&workspace_root, &workspace_root, requested)?;
    if resolved.scope != ToolSubjectScope::Workspace {
        bail!("delete_file path is outside workspace: {requested}");
    }
    let requested_path = Path::new(requested);
    let path = if requested_path.is_absolute() {
        lexically_normalize_path(requested_path)?
    } else {
        lexically_normalize_path(&workspace_root.join(requested_path))?
    };
    Ok(DeleteFileTarget {
        path,
        display_path: requested.to_owned(),
    })
}

fn validate_delete_file_target(path: &Path, display_path: &str) -> Result<fs::Metadata> {
    let symlink_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if symlink_metadata.file_type().is_symlink() {
        bail!("delete_file does not support symlink paths: {display_path}");
    }
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("delete_file only supports regular files: {display_path}");
    }
    Ok(metadata)
}

fn resolve_tool_path_from_base(
    workspace_root: &Path,
    base_dir: &Path,
    requested: &str,
) -> Result<ResolvedToolPath> {
    let requested_path = Path::new(requested);
    let lexical_target = if requested_path.is_absolute() {
        lexically_normalize_path(requested_path)?
    } else {
        lexically_normalize_path(&base_dir.join(requested_path))?
    };
    let canonical = resolve_existing_prefix(&lexical_target)?;
    let scope = if canonical.starts_with(workspace_root) {
        ToolSubjectScope::Workspace
    } else {
        ToolSubjectScope::External
    };
    let normalized = match scope {
        ToolSubjectScope::Workspace => {
            let relative = relativize(workspace_root, &canonical)?;
            if relative.is_empty() {
                ".".to_owned()
            } else {
                relative
            }
        }
        ToolSubjectScope::External => canonical.to_string_lossy().to_string(),
        ToolSubjectScope::Unknown => canonical.to_string_lossy().to_string(),
    };
    Ok(ResolvedToolPath {
        original: requested.to_owned(),
        normalized,
        canonical,
        scope,
    })
}

fn canonical_workspace_root(workspace_root: &Path) -> Result<PathBuf> {
    fs::canonicalize(workspace_root).with_context(|| {
        format!(
            "failed to resolve workspace root {}",
            workspace_root.display()
        )
    })
}

fn absolute_path_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn lexically_normalize_path(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) => bail!("platform path prefixes are not supported"),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        Ok(PathBuf::from("."))
    } else {
        Ok(normalized)
    }
}

fn resolve_existing_prefix(absolute_path: &Path) -> Result<PathBuf> {
    let components = absolute_path
        .components()
        .map(|component| component.as_os_str().to_os_string())
        .collect::<Vec<OsString>>();
    let mut resolved = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        let candidate = if resolved.as_os_str().is_empty() {
            PathBuf::from(component)
        } else {
            resolved.join(component)
        };
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                resolved = fs::canonicalize(&candidate)
                    .with_context(|| format!("failed to resolve {}", candidate.display()))?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let mut missing_path = candidate;
                for remaining in components.iter().skip(index + 1) {
                    missing_path.push(remaining);
                }
                return lexically_normalize_path(&missing_path);
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", candidate.display()));
            }
        }
    }
    Ok(resolved)
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

fn shell_command_permission_operation(command: &str) -> ToolOperation {
    if shell_command_is_destructive(command) {
        ToolOperation::ExecuteDestructiveCommand
    } else if bash_command_is_safe_readonly(command) {
        ToolOperation::ExecuteReadOnlyCommand
    } else {
        ToolOperation::ExecuteUnknownCommand
    }
}

fn terminal_input_permission_operation(input: &str) -> ToolOperation {
    if shell_command_is_destructive(input) {
        ToolOperation::ExecuteDestructiveCommand
    } else {
        ToolOperation::SendTerminalInput
    }
}

fn shell_command_is_destructive(command: &str) -> bool {
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

fn shell_segment_is_destructive(words: &[String]) -> bool {
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

fn shell_segment_command_and_args(words: &[String]) -> Option<(&str, &[String])> {
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

fn is_shell_assignment(word: &str) -> bool {
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

fn shell_command_basename(command: &str) -> &str {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(command)
}

fn shell_segment_has_overwrite_redirection(words: &[String]) -> bool {
    words
        .iter()
        .any(|word| is_overwrite_redirection_operator(word) || overwrite_redirection_target(word))
}

fn is_overwrite_redirection_operator(word: &str) -> bool {
    matches!(word, ">" | "1>" | "2>" | "&>")
}

fn overwrite_redirection_target(word: &str) -> bool {
    ["1>", "2>", "&>", ">"].iter().any(|prefix| {
        word.strip_prefix(prefix)
            .is_some_and(|target| !target.is_empty())
    })
}

fn find_segment_is_destructive(words: &[String]) -> bool {
    words.iter().enumerate().any(|(index, word)| {
        word == "-delete"
            || matches!(word.as_str(), "-exec" | "-execdir")
                && words
                    .get(index + 1)
                    .map(|command| shell_command_basename(command) == "rm")
                    .unwrap_or(false)
    })
}

fn git_segment_is_destructive(words: &[String]) -> bool {
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

fn shell_invocation_is_destructive(words: &[String]) -> bool {
    words.windows(2).any(|pair| {
        matches!(pair[0].as_str(), "-c" | "-lc") && shell_command_is_destructive(&pair[1])
    })
}

fn bash_command_is_safe_readonly(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() || contains_unsupported_safe_shell_syntax(trimmed) {
        return false;
    }

    let tokens = tokenize_shell_subject_words(trimmed);
    if tokens.is_empty() {
        return false;
    }

    let mut segment = Vec::new();
    for token in tokens {
        if matches!(token.as_str(), "&&" | "||" | ";") {
            if !bash_segment_is_safe_readonly(&segment) {
                return false;
            }
            segment.clear();
        } else {
            segment.push(token);
        }
    }
    bash_segment_is_safe_readonly(&segment)
}

fn contains_unsupported_safe_shell_syntax(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            '|' | '>' | '<' | '$' | '`' | '(' | ')' | '*' | '?' | '[' | ']'
        )
    })
}

fn bash_segment_is_safe_readonly(words: &[String]) -> bool {
    let Some(command) = words.first().map(String::as_str) else {
        return false;
    };

    if words
        .iter()
        .skip(1)
        .any(|word| is_redirection_operator(word) || redirection_target(word).is_some())
    {
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

fn is_help_or_version_query(words: &[String]) -> bool {
    words.len() == 2
        && matches!(
            words[1].as_str(),
            "--version" | "-V" | "--help" | "-h" | "help"
        )
}

fn find_segment_is_safe_readonly(words: &[String]) -> bool {
    !words.iter().skip(1).any(|word| {
        matches!(
            word.as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir" | "-delete" | "-fprint" | "-fprintf" | "-fls"
        )
    })
}

fn git_segment_is_safe_readonly(words: &[String]) -> bool {
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

fn bash_path_subjects(workspace_root: &Path, command: &str) -> Result<Vec<ToolSubject>> {
    let workspace_root = canonical_workspace_root(workspace_root)?;
    bash_path_subjects_from_cwd(&workspace_root, &workspace_root, command)
}

fn bash_path_subjects_from_cwd(
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

fn collect_bash_segment_subjects(
    workspace_root: &Path,
    cwd: &mut PathBuf,
    words: &[String],
    subjects: &mut Vec<ToolSubject>,
) -> Result<()> {
    if words.is_empty() {
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
            subjects.push(shell_path_subject(workspace_root, cwd, target)?);
        } else if command == "dd" && word.starts_with("of=") && word.len() > 3 {
            subjects.push(shell_path_subject(workspace_root, cwd, &word[3..])?);
        } else if is_redirection_operator(word) {
            if let Some(target) = words.get(index + 1) {
                subjects.push(shell_path_subject(workspace_root, cwd, target)?);
                index += 1;
            }
        } else if is_path_argument(command, word) {
            subjects.push(shell_path_subject(workspace_root, cwd, word)?);
        }
        index += 1;
    }
    Ok(())
}

fn shell_path_subject(workspace_root: &Path, cwd: &Path, requested: &str) -> Result<ToolSubject> {
    resolve_tool_path_from_base(workspace_root, cwd, requested).map(resolved_tool_path_subject)
}

fn resolved_tool_path_subject(resolved: ResolvedToolPath) -> ToolSubject {
    ToolSubject::path_with_scope(
        resolved.original,
        resolved.normalized,
        Some(resolved.canonical),
        resolved.scope,
    )
}

fn tokenize_shell_subject_words(command: &str) -> Vec<String> {
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
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn is_redirection_operator(word: &str) -> bool {
    matches!(
        word,
        ">" | ">>" | "<" | "<<" | "2>" | "2>>" | "&>" | "&>>" | "1>" | "1>>"
    )
}

fn redirection_target(word: &str) -> Option<&str> {
    for prefix in [">>", ">", "<", "2>>", "2>", "&>>", "&>", "1>>", "1>"] {
        if let Some(target) = word
            .strip_prefix(prefix)
            .filter(|target| !target.is_empty())
        {
            return Some(target);
        }
    }
    None
}

fn is_path_argument(command: &str, word: &str) -> bool {
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

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
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
#[path = "tests/lib_tests.rs"]
mod tests;
