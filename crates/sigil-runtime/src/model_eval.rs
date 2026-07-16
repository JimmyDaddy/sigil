use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::ToolRegistryScope;

mod campaign;
pub use campaign::*;
mod report;
pub use report::*;
mod verification;
pub use verification::*;

pub const MODEL_EVAL_FIXTURE_SCHEMA_VERSION: u16 = 1;
pub const MODEL_EVAL_MAX_FILES: usize = 32;
pub const MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES: u64 = 1024 * 1024;
pub const MODEL_EVAL_MAX_PROMPT_BYTES: u64 = 16 * 1024;
pub const MODEL_EVAL_MAX_TURNS: u32 = 16;
pub const MODEL_EVAL_MAX_OUTPUT_TOKENS: u32 = 32 * 1024;
pub const MODEL_EVAL_MAX_CHECKS: usize = 4;
pub const MODEL_EVAL_MAX_ASSERTIONS: usize = 8;
pub const MODEL_EVAL_MAX_CHECK_TIMEOUT_MS: u64 = 60_000;

const MODEL_EVAL_ALLOWED_TOOLS: &[&str] = &["edit_file", "read_file", "write_file"];

/// Strict, committed definition for one generated model-eval workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ModelEvalFixtureManifest {
    pub schema_version: u16,
    pub id: String,
    pub prompt_file: PathBuf,
    pub prompt_sha256: String,
    pub allowed_tools: Vec<String>,
    pub max_turns: u32,
    pub max_output_tokens: u32,
    pub expected_terminal: Vec<ModelEvalExpectedTerminal>,
    pub expected_verification: Vec<ModelEvalExpectedVerification>,
    pub files: Vec<ModelEvalFixtureFile>,
    pub checks: Vec<ModelEvalFixtureCheck>,
    pub assertions: Vec<ModelEvalFixtureAssertion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_run_mutation: Option<ModelEvalPostRunMutation>,
}

/// One immutable source file copied into a generated fixture workspace.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ModelEvalFixtureFile {
    pub path: PathBuf,
    pub source: PathBuf,
    pub sha256: String,
}

/// One trusted verification command committed with a fixture.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ModelEvalFixtureCheck {
    pub id: String,
    pub command: Vec<String>,
    pub timeout_ms: u64,
}

/// Harness-owned mutation used only to prove verification staleness after a model run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ModelEvalPostRunMutation {
    pub path: PathBuf,
    pub old_text: String,
    pub new_text: String,
}

/// One committed safety or outcome assertion evaluated without assistant-text authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct ModelEvalFixtureAssertion {
    pub id: String,
    #[serde(flatten)]
    pub assertion: ModelEvalFixtureAssertionKind,
}

/// Supported V1 fixture assertions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
pub enum ModelEvalFixtureAssertionKind {
    FileContains {
        path: PathBuf,
        text: String,
    },
    ToolStatus {
        tool_name: String,
        status: ModelEvalExpectedToolStatus,
    },
    ToolNotCalled {
        tool_name: String,
    },
    PathAbsent {
        path: PathBuf,
    },
    WorkspaceSourceUnchanged,
}

/// Terminal tool status accepted by a fixture assertion.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelEvalExpectedToolStatus {
    Succeeded,
    Failed,
    Denied,
    Interrupted,
}

/// Allowed agent terminal buckets for one fixture.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelEvalExpectedTerminal {
    Completed,
    Blocked,
    Failed,
}

/// Allowed verification buckets for one fixture.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelEvalExpectedVerification {
    Passed,
    Stale,
    Missing,
    NotApplicable,
}

/// Fully validated fixture source. No provider or workspace mutation has happened yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedModelEvalFixture {
    pub source_root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest_digest: String,
    pub prompt: String,
    pub manifest: ModelEvalFixtureManifest,
}

/// Receipt for a newly generated isolated workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedModelEvalFixture {
    pub fixture_id: String,
    pub workspace_root: PathBuf,
    pub manifest_digest: String,
    pub tree_digest: String,
    pub prompt: String,
    pub tool_scope: ToolRegistryScope,
    pub max_turns: u32,
    pub max_output_tokens: u32,
    pub checks: Vec<ModelEvalFixtureCheck>,
    pub assertions: Vec<ModelEvalFixtureAssertion>,
    pub fixture_files: Vec<PathBuf>,
    pub expected_terminal: Vec<ModelEvalExpectedTerminal>,
    pub expected_verification: Vec<ModelEvalExpectedVerification>,
    pub post_run_mutation: Option<ModelEvalPostRunMutation>,
}

/// Loads and fully validates one committed model-eval fixture directory.
///
/// # Errors
///
/// Returns an error for malformed manifests, unsupported bounds, unsafe paths, symlinks, unknown
/// tools or commands, and any source digest mismatch.
pub fn load_model_eval_fixture(source_root: impl AsRef<Path>) -> Result<LoadedModelEvalFixture> {
    let source_root = canonical_regular_directory(source_root.as_ref())?;
    let manifest_path = source_root.join("fixture.toml");
    let manifest_bytes = read_bounded_regular_file(
        &manifest_path,
        MODEL_EVAL_MAX_PROMPT_BYTES,
        "model eval fixture manifest",
    )?;
    let manifest: ModelEvalFixtureManifest = toml::from_str(
        std::str::from_utf8(&manifest_bytes).context("model eval fixture manifest is not UTF-8")?,
    )
    .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    validate_manifest(&manifest)?;

    let prompt_path = resolve_source_path(&source_root, &manifest.prompt_file)?;
    let prompt_bytes = read_bounded_regular_file(
        &prompt_path,
        MODEL_EVAL_MAX_PROMPT_BYTES,
        "model eval fixture prompt",
    )?;
    validate_digest("prompt_sha256", &manifest.prompt_sha256, &prompt_bytes)?;
    let prompt = String::from_utf8(prompt_bytes)
        .with_context(|| format!("model eval prompt is not UTF-8: {}", prompt_path.display()))?;
    if prompt.trim().is_empty() {
        bail!("model eval fixture prompt must not be empty");
    }

    let mut total_bytes = 0_u64;
    for file in &manifest.files {
        let source_path = resolve_source_path(&source_root, &file.source)?;
        let bytes = read_bounded_regular_file(
            &source_path,
            MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES,
            "model eval fixture source",
        )?;
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .context("model eval fixture source byte count overflowed")?;
        if total_bytes > MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES {
            bail!(
                "model eval fixture source exceeds {} bytes",
                MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES
            );
        }
        validate_digest("file sha256", &file.sha256, &bytes)?;
    }

    Ok(LoadedModelEvalFixture {
        source_root,
        manifest_path,
        manifest_digest: sha256_digest(&manifest_bytes),
        prompt,
        manifest,
    })
}

/// Materializes a validated fixture into one new workspace directory.
///
/// The destination must not exist. Publication uses create-new files inside a directory created by
/// this function, so a caller cannot accidentally overwrite a previous repetition.
///
/// # Errors
///
/// Returns an error if the destination already exists, a source changes after load, or any
/// destination file cannot be created and synchronized.
pub fn materialize_model_eval_fixture(
    fixture: &LoadedModelEvalFixture,
    workspace_root: impl AsRef<Path>,
) -> Result<MaterializedModelEvalFixture> {
    let workspace_root = workspace_root.as_ref();
    if workspace_root.exists() {
        bail!(
            "model eval workspace destination already exists: {}",
            workspace_root.display()
        );
    }
    let parent = workspace_root
        .parent()
        .context("model eval workspace has no parent")?;
    let parent = canonical_regular_directory(parent)?;
    let leaf = workspace_root
        .file_name()
        .context("model eval workspace has no final component")?;
    let workspace_root = parent.join(leaf);
    fs::create_dir(&workspace_root)
        .with_context(|| format!("failed to create {}", workspace_root.display()))?;

    let materialize_result = materialize_files(fixture, &workspace_root);
    if materialize_result.is_err() {
        let _ = fs::remove_dir_all(&workspace_root);
    }
    let tree_digest = materialize_result?;
    sync_directory(&workspace_root)?;
    sync_directory(&parent)?;

    Ok(MaterializedModelEvalFixture {
        fixture_id: fixture.manifest.id.clone(),
        workspace_root,
        manifest_digest: fixture.manifest_digest.clone(),
        tree_digest,
        prompt: fixture.prompt.clone(),
        tool_scope: ToolRegistryScope::from_names_and_prefixes(
            fixture.manifest.allowed_tools.iter().cloned(),
            std::iter::empty::<String>(),
        ),
        max_turns: fixture.manifest.max_turns,
        max_output_tokens: fixture.manifest.max_output_tokens,
        checks: fixture.manifest.checks.clone(),
        assertions: fixture.manifest.assertions.clone(),
        fixture_files: fixture
            .manifest
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect(),
        expected_terminal: fixture.manifest.expected_terminal.clone(),
        expected_verification: fixture.manifest.expected_verification.clone(),
        post_run_mutation: fixture.manifest.post_run_mutation.clone(),
    })
}

fn validate_manifest(manifest: &ModelEvalFixtureManifest) -> Result<()> {
    if manifest.schema_version != MODEL_EVAL_FIXTURE_SCHEMA_VERSION {
        bail!(
            "unsupported model eval fixture schema version: {}",
            manifest.schema_version
        );
    }
    validate_id("fixture id", &manifest.id)?;
    validate_relative_path("prompt_file", &manifest.prompt_file)?;
    validate_sha256_shape("prompt_sha256", &manifest.prompt_sha256)?;

    if manifest.allowed_tools.is_empty() {
        bail!("model eval fixture tool scope must not be empty");
    }
    if manifest.allowed_tools.len() > MODEL_EVAL_ALLOWED_TOOLS.len() {
        bail!("model eval fixture has too many allowed tools");
    }
    let mut tools = BTreeSet::new();
    for tool in &manifest.allowed_tools {
        if !MODEL_EVAL_ALLOWED_TOOLS.contains(&tool.as_str()) {
            bail!("model eval fixture contains unsupported tool: {tool}");
        }
        if !tools.insert(tool.as_str()) {
            bail!("model eval fixture contains duplicate tool: {tool}");
        }
    }

    if manifest.max_turns == 0 || manifest.max_turns > MODEL_EVAL_MAX_TURNS {
        bail!(
            "model eval max_turns must be between 1 and {}",
            MODEL_EVAL_MAX_TURNS
        );
    }
    if manifest.max_output_tokens == 0 || manifest.max_output_tokens > MODEL_EVAL_MAX_OUTPUT_TOKENS
    {
        bail!(
            "model eval max_output_tokens must be between 1 and {}",
            MODEL_EVAL_MAX_OUTPUT_TOKENS
        );
    }
    if manifest.expected_terminal.is_empty() || manifest.expected_verification.is_empty() {
        bail!("model eval fixture expected result sets must not be empty");
    }
    if manifest.files.is_empty() || manifest.files.len() > MODEL_EVAL_MAX_FILES {
        bail!(
            "model eval fixture must contain between 1 and {} files",
            MODEL_EVAL_MAX_FILES
        );
    }

    let mut destinations = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for file in &manifest.files {
        validate_relative_path("file path", &file.path)?;
        validate_relative_path("file source", &file.source)?;
        validate_sha256_shape("file sha256", &file.sha256)?;
        if !destinations.insert(file.path.clone()) {
            bail!(
                "model eval fixture contains duplicate destination: {}",
                file.path.display()
            );
        }
        if !sources.insert(file.source.clone()) {
            bail!(
                "model eval fixture contains duplicate source: {}",
                file.source.display()
            );
        }
    }

    if manifest.checks.is_empty() || manifest.checks.len() > MODEL_EVAL_MAX_CHECKS {
        bail!(
            "model eval fixture must contain between 1 and {} checks",
            MODEL_EVAL_MAX_CHECKS
        );
    }
    let mut check_ids = BTreeSet::new();
    for check in &manifest.checks {
        validate_id("check id", &check.id)?;
        if !check_ids.insert(check.id.as_str()) {
            bail!(
                "model eval fixture contains duplicate check id: {}",
                check.id
            );
        }
        validate_check_command(&check.command)?;
        if check.timeout_ms == 0 || check.timeout_ms > MODEL_EVAL_MAX_CHECK_TIMEOUT_MS {
            bail!(
                "model eval check timeout must be between 1 and {} milliseconds",
                MODEL_EVAL_MAX_CHECK_TIMEOUT_MS
            );
        }
    }

    if let Some(mutation) = &manifest.post_run_mutation {
        validate_relative_path("post_run_mutation path", &mutation.path)?;
        if !destinations.contains(&mutation.path) {
            bail!("model eval post-run mutation must target a declared fixture file");
        }
        if mutation.old_text.is_empty()
            || mutation.old_text.len() > 4096
            || mutation.new_text.len() > 4096
            || mutation.old_text == mutation.new_text
        {
            bail!("model eval post-run mutation text is invalid");
        }
    }
    if manifest.assertions.is_empty() || manifest.assertions.len() > MODEL_EVAL_MAX_ASSERTIONS {
        bail!(
            "model eval fixture must contain between 1 and {} assertions",
            MODEL_EVAL_MAX_ASSERTIONS
        );
    }
    let mut assertion_ids = BTreeSet::new();
    for assertion in &manifest.assertions {
        validate_id("assertion id", &assertion.id)?;
        if !assertion_ids.insert(assertion.id.as_str()) {
            bail!(
                "model eval fixture contains duplicate assertion id: {}",
                assertion.id
            );
        }
        match &assertion.assertion {
            ModelEvalFixtureAssertionKind::FileContains { path, text } => {
                validate_relative_path("assertion file path", path)?;
                if !destinations.contains(path) || text.is_empty() || text.len() > 4096 {
                    bail!("model eval file assertion is invalid");
                }
            }
            ModelEvalFixtureAssertionKind::ToolStatus { tool_name, .. } => {
                if !manifest
                    .allowed_tools
                    .iter()
                    .any(|allowed| allowed == tool_name)
                {
                    bail!("model eval tool assertion references an unavailable tool");
                }
            }
            ModelEvalFixtureAssertionKind::ToolNotCalled { tool_name } => {
                if tool_name.is_empty() || tool_name.len() > 64 {
                    bail!("model eval tool assertion has an invalid tool name");
                }
            }
            ModelEvalFixtureAssertionKind::PathAbsent { path } => {
                validate_bounded_parent_path("assertion absent path", path)?;
            }
            ModelEvalFixtureAssertionKind::WorkspaceSourceUnchanged => {}
        }
    }
    Ok(())
}

fn validate_check_command(command: &[String]) -> Result<()> {
    let allowed = [
        ["cargo", "check", "--quiet"].as_slice(),
        ["cargo", "test", "--quiet"].as_slice(),
    ];
    if allowed.iter().any(|candidate| {
        candidate.len() == command.len()
            && candidate
                .iter()
                .zip(command)
                .all(|(expected, observed)| expected == observed)
    }) {
        return Ok(());
    }
    bail!("model eval fixture check command is not in the V1 allowlist");
}

fn validate_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("{field} must use 1-64 lowercase ASCII letters, digits, or hyphens");
    }
    Ok(())
}

fn validate_relative_path(field: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() || path.as_os_str().len() > 240 {
        bail!("{field} must be a bounded relative path");
    }
    for component in path.components() {
        match component {
            Component::Normal(_) => {}
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => bail!("{field} contains an unsafe path component"),
        }
    }
    Ok(())
}

fn validate_bounded_parent_path(field: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() || path.as_os_str().len() > 240 {
        bail!("{field} must be a bounded relative path");
    }
    let mut components = path.components();
    if components.next() != Some(Component::ParentDir) {
        bail!("{field} must start with exactly one parent component");
    }
    let mut normal_count = 0_usize;
    for component in components {
        match component {
            Component::Normal(_) => normal_count += 1,
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => bail!("{field} contains an unsafe path component"),
        }
    }
    if normal_count == 0 {
        bail!("{field} must name a path below the fixture parent");
    }
    Ok(())
}

fn canonical_regular_directory(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!(
            "model eval path is not a regular directory: {}",
            path.display()
        );
    }
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))
}

fn resolve_source_path(source_root: &Path, relative: &Path) -> Result<PathBuf> {
    validate_relative_path("fixture source path", relative)?;
    let joined = source_root.join(relative);
    let metadata = fs::symlink_metadata(&joined)
        .with_context(|| format!("failed to inspect {}", joined.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!(
            "model eval source is not a regular file: {}",
            joined.display()
        );
    }
    let canonical = joined
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {}", joined.display()))?;
    if !canonical.starts_with(source_root) {
        bail!(
            "model eval source escapes fixture root: {}",
            joined.display()
        );
    }
    Ok(canonical)
}

fn read_bounded_regular_file(path: &Path, max_bytes: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("{label} is not a regular file: {}", path.display());
    }
    if metadata.len() > max_bytes {
        bail!("{label} exceeds {max_bytes} bytes: {}", path.display());
    }
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() as u64 != metadata.len() || bytes.len() as u64 > max_bytes {
        bail!("{label} changed while reading: {}", path.display());
    }
    Ok(bytes)
}

fn validate_sha256_shape(field: &str, digest: &str) -> Result<()> {
    if digest.len() != 71
        || !digest.starts_with("sha256:")
        || !digest[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("{field} must contain a sha256 digest");
    }
    Ok(())
}

fn validate_digest(field: &str, expected: &str, bytes: &[u8]) -> Result<()> {
    validate_sha256_shape(field, expected)?;
    let observed = sha256_digest(bytes);
    if observed != expected {
        bail!("{field} mismatch: expected {expected}, observed {observed}");
    }
    Ok(())
}

fn materialize_files(fixture: &LoadedModelEvalFixture, workspace_root: &Path) -> Result<String> {
    let mut files = fixture.manifest.files.clone();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    let mut tree_hasher = Sha256::new();
    tree_hasher.update(b"sigil-model-eval-tree-v1\0");

    for file in files {
        let source_path = resolve_source_path(&fixture.source_root, &file.source)?;
        let bytes = read_bounded_regular_file(
            &source_path,
            MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES,
            "model eval fixture source",
        )?;
        validate_digest("file sha256", &file.sha256, &bytes)?;

        let destination = workspace_root.join(&file.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .with_context(|| format!("failed to create {}", destination.display()))?;
        output
            .write_all(&bytes)
            .with_context(|| format!("failed to write {}", destination.display()))?;
        output
            .sync_all()
            .with_context(|| format!("failed to sync {}", destination.display()))?;

        let path = file.path.to_string_lossy();
        tree_hasher.update((path.len() as u64).to_le_bytes());
        tree_hasher.update(path.as_bytes());
        tree_hasher.update((bytes.len() as u64).to_le_bytes());
        tree_hasher.update(&bytes);
    }
    Ok(format!("sha256:{:x}", tree_hasher.finalize()))
}

/// Recomputes the digest of committed fixture source paths, excluding generated check artifacts.
pub fn current_model_eval_fixture_tree_digest(
    fixture: &MaterializedModelEvalFixture,
) -> Result<String> {
    let mut paths = fixture.fixture_files.clone();
    paths.sort();
    let mut tree_hasher = Sha256::new();
    tree_hasher.update(b"sigil-model-eval-tree-v1\0");
    for path in paths {
        let absolute = fixture.workspace_root.join(&path);
        let bytes = read_bounded_regular_file(
            &absolute,
            MODEL_EVAL_MAX_TOTAL_SOURCE_BYTES,
            "materialized model eval fixture source",
        )?;
        let rendered = path.to_string_lossy();
        tree_hasher.update((rendered.len() as u64).to_le_bytes());
        tree_hasher.update(rendered.as_bytes());
        tree_hasher.update((bytes.len() as u64).to_le_bytes());
        tree_hasher.update(&bytes);
    }
    Ok(format!("sha256:{:x}", tree_hasher.finalize()))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    let directory = fs::File::open(path)
        .with_context(|| format!("failed to open directory {}", path.display()))?;
    directory
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    // Materialized files are individually synced before publication. Directory fsync is not
    // available through Rust's portable Windows filesystem API.
    Ok(())
}

fn sha256_digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
