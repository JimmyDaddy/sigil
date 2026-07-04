use std::{
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sigil_kernel::{
    ChangeSet, ChangeSetFile, ChangeSetFileAction, ChangeSetFileResult, ChangeSetFileResultStatus,
    ChangeSetId, ChangeSetResult, ChangeSetResultStatus, ChangeSetRisk, ChangeSetValidation,
    ChangeSetValidationKind, ChangeSetValidationStatus, CommittedFileMutation, FileType,
    MutationBatchId, MutationBatchStatus, MutationEventRecorder, MutationSubject, Tool, ToolAccess,
    ToolCategory, ToolContext, ToolDiffStats, ToolErrorKind, ToolPreview, ToolPreviewCapability,
    ToolPreviewFile, ToolResult, ToolResultMeta, ToolSpec, ToolSubject,
    delete_file_with_mutation_in_batch, write_file_with_mutation_in_batch,
};

use crate::{
    constants::{
        CHANGESET_ARTIFACT_ROOT, CHANGESET_PREVIEW_DIFF_FILE, CHANGESET_REVERSE_DIFF_FILE,
        DEFAULT_CHANGESET_SUMMARY_LIMIT_BYTES,
    },
    path::{
        absolute_path_from, canonical_workspace_root, lexically_normalize_path, relativize,
        resolve_existing_prefix, tool_path_subject,
    },
    support::{limit_text_head_tail, render_unified_diff, run_blocking_io, sha256_hex},
};

pub(crate) struct ApplyChangeSetTool {
    pub(crate) artifact_root: PathBuf,
    pub(crate) artifact_label_root: PathBuf,
}

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
pub(crate) struct ChangeSetArtifactPaths {
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
pub(crate) struct ApplyChangeSetArgs {
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
pub(crate) struct ApplyChangeSetFileArg {
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
    #[serde(default)]
    pub before_hash: Option<String>,
    #[serde(default)]
    pub before_mtime_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ApplyChangeSetPlan {
    pub(crate) change_set: ChangeSet,
    pub(crate) files: Vec<PlannedChangeSetFile>,
    pub(crate) preview_diff: String,
    pub(crate) reverse_diff: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PlannedChangeSetFile {
    pub(crate) path: String,
    pub(crate) absolute_path: PathBuf,
    pub(crate) action: ChangeSetFileAction,
    pub(crate) after_content: Option<String>,
    pub(crate) preview_diff: String,
    pub(crate) reverse_diff: String,
    pub(crate) validations: Vec<ChangeSetValidation>,
}

#[derive(Debug, Clone)]
pub(crate) struct ApplyChangeSetPlanError {
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
pub(crate) struct PlannedChangeSetFailure {
    path: String,
    action: ChangeSetFileAction,
    validations: Vec<ChangeSetValidation>,
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedChangePath {
    normalized: String,
    absolute: PathBuf,
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

pub(crate) fn parse_apply_changeset_args(args: &Value) -> Result<ApplyChangeSetArgs> {
    serde_json::from_value(args.clone()).context("invalid apply_changeset arguments")
}

pub(crate) fn build_apply_changeset_plan(
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

pub(crate) fn plan_changeset_file(
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

pub(crate) fn apply_changeset_plan(
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

pub(crate) fn apply_planned_changeset_file(
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

pub(crate) fn apply_changeset_error_result(
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

pub(crate) fn apply_changeset_details(
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

pub(crate) fn validation_plan_error(
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

pub(crate) fn file_failure(
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
pub(crate) struct TextFileSnapshot {
    content: String,
    hash: String,
    mtime_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct ValidationIssue {
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

pub(crate) fn read_text_snapshot(
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

pub(crate) fn validate_text_content(content: &str) -> std::result::Result<(), ValidationIssue> {
    if content.contains('\0') {
        return Err(ValidationIssue::new(
            ChangeSetValidationKind::Binary,
            "binary_file: proposed content contains NUL bytes",
        ));
    }
    Ok(())
}

pub(crate) fn validate_expected_hash(
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

pub(crate) fn validate_expected_mtime(
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

pub(crate) fn append_failed_validation(
    mut validations: Vec<ChangeSetValidation>,
    issue: ValidationIssue,
) -> Vec<ChangeSetValidation> {
    validations.push(validation_failed(issue.kind, issue.message));
    validations
}

pub(crate) fn validation_passed(
    kind: ChangeSetValidationKind,
    message: Option<String>,
) -> ChangeSetValidation {
    ChangeSetValidation {
        kind,
        status: ChangeSetValidationStatus::Passed,
        message,
    }
}

pub(crate) fn validation_failed(
    kind: ChangeSetValidationKind,
    message: impl Into<String>,
) -> ChangeSetValidation {
    ChangeSetValidation {
        kind,
        status: ChangeSetValidationStatus::Failed,
        message: Some(message.into()),
    }
}

pub(crate) fn resolve_workspace_change_path(
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

pub(crate) fn metadata_mtime_ms(metadata: &fs::Metadata) -> Option<u64> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(duration.as_millis()).ok()
}
