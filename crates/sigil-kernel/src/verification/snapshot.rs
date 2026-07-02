use super::*;

/// Derives the stable workspace id used by verification snapshots for a workspace root.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized.
pub fn stable_workspace_id(workspace_root: impl AsRef<Path>) -> Result<WorkspaceId> {
    let workspace_root = workspace_root.as_ref();
    let canonical = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    Ok(format!("workspace:{digest:x}"))
}

/// Durable control entry recording a verification policy update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationPolicyChangedEntry {
    pub scope: EvidenceScope,
    pub policy: VerificationPolicy,
    pub policy_hash: PolicyHash,
    pub source_event_id: EventId,
}

impl VerificationPolicyChangedEntry {
    /// Builds a policy entry and computes its content hash.
    ///
    /// # Errors
    ///
    /// Returns an error if the policy cannot be hashed deterministically.
    pub fn new(
        scope: EvidenceScope,
        policy: VerificationPolicy,
        source_event_id: impl Into<EventId>,
    ) -> Result<Self> {
        let policy_hash = policy.stable_hash()?;
        Ok(Self {
            scope,
            policy,
            policy_hash,
            source_event_id: source_event_id.into(),
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    WorkspaceWrite,
    ExternalWrite,
    Network,
    Unknown,
}

impl ToolEffect {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::WorkspaceWrite => "workspace_write",
            Self::ExternalWrite => "external_write",
            Self::Network => "network",
            Self::Unknown => "unknown",
        }
    }

    pub fn may_mutate_workspace(self) -> bool {
        matches!(
            self,
            Self::WorkspaceWrite | Self::ExternalWrite | Self::Unknown
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "revision")]
pub enum WorkspaceKnowledge {
    Clean(WorkspaceRevision),
    Dirty(WorkspaceRevision),
    UnknownDirty,
}

impl WorkspaceKnowledge {
    pub fn is_unknown_dirty(&self) -> bool {
        matches!(self, Self::UnknownDirty)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct VerificationBinding {
    pub workspace_id: WorkspaceId,
    pub workspace_snapshot_id: WorkspaceSnapshotId,
    pub verification_scope_hash: VerificationScopeHash,
    pub check_spec_hash: String,
    pub environment_fingerprint: EnvironmentFingerprint,
    pub sandbox_profile_hash: SandboxProfileHash,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend: Option<ExecutionBackendKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_backend_capabilities: Option<ExecutionBackendCapabilities>,
    #[serde(default)]
    pub execution_network: ExecutionNetworkReceipt,
    pub workspace_trust_snapshot_id: WorkspaceTrustSnapshotId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_event_id: Option<EventId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox_decision_id: Option<EventId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSnapshotManifestV1 {
    pub workspace_id: WorkspaceId,
    pub scope_hash: VerificationScopeHash,
    pub entries: Vec<WorkspaceSnapshotEntry>,
}

/// Result of building a verification-scope workspace snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSnapshotBuild {
    pub manifest: WorkspaceSnapshotManifestV1,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_snapshot_id: Option<WorkspaceSnapshotId>,
    pub workspace_knowledge: WorkspaceKnowledge,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_dirty_evidence: Option<WorkspaceMutationEvidence>,
}

/// Builds a content-bound workspace snapshot for a verification scope.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized or when scope patterns are
/// invalid. Incomplete per-file coverage is represented as `UnknownDirty` in the returned value.
pub fn build_workspace_snapshot(
    workspace_root: impl AsRef<Path>,
    workspace_id: impl Into<WorkspaceId>,
    scope: &VerificationScope,
    workspace_revision: WorkspaceRevision,
) -> Result<WorkspaceSnapshotBuild> {
    build_workspace_snapshot_inner(
        workspace_root.as_ref(),
        workspace_id.into(),
        scope,
        workspace_revision,
        None,
    )
}

/// Builds a snapshot and records incomplete coverage as auditable unknown-dirty evidence.
///
/// # Errors
///
/// Returns an error when the workspace root cannot be canonicalized or when scope patterns are
/// invalid.
pub fn build_workspace_snapshot_for_event(
    workspace_root: impl AsRef<Path>,
    workspace_id: impl Into<WorkspaceId>,
    scope: &VerificationScope,
    workspace_revision: WorkspaceRevision,
    source_event_id: impl Into<EventId>,
    recorded_at_stream_sequence: u64,
) -> Result<WorkspaceSnapshotBuild> {
    if recorded_at_stream_sequence == 0 {
        bail!("workspace snapshot source stream sequence must be non-zero");
    }
    build_workspace_snapshot_inner(
        workspace_root.as_ref(),
        workspace_id.into(),
        scope,
        workspace_revision,
        Some((source_event_id.into(), recorded_at_stream_sequence)),
    )
}

fn build_workspace_snapshot_inner(
    workspace_root: &Path,
    workspace_id: WorkspaceId,
    scope: &VerificationScope,
    workspace_revision: WorkspaceRevision,
    source_event: Option<(EventId, u64)>,
) -> Result<WorkspaceSnapshotBuild> {
    let canonical_root = fs::canonicalize(workspace_root)
        .with_context(|| format!("failed to canonicalize {}", workspace_root.display()))?;
    let include_set = glob_set(&scope.include)?;
    let exclude_patterns = effective_exclude_patterns(scope);
    let exclude_set = glob_set(&exclude_patterns)?;
    let mut entries = Vec::new();
    if let Some(paths) = git_snapshot_paths(&canonical_root, scope) {
        collect_snapshot_entries_for_paths(
            &canonical_root,
            paths,
            &include_set,
            &exclude_set,
            scope.max_file_bytes,
            &mut entries,
        );
    } else {
        collect_snapshot_entries(
            &canonical_root,
            &canonical_root,
            &include_set,
            &exclude_set,
            scope.max_file_bytes,
            &mut entries,
        )?;
    }
    add_missing_literal_includes(&canonical_root, scope, &mut entries)?;
    entries.sort_by(|left, right| left.normalized_path.cmp(&right.normalized_path));
    entries.dedup_by(|left, right| left.normalized_path == right.normalized_path);
    let manifest = WorkspaceSnapshotManifestV1 {
        workspace_id,
        scope_hash: scope.scope_hash.clone(),
        entries,
    };
    let workspace_snapshot_id = manifest.workspace_snapshot_id().ok();
    let unknown_dirty_evidence = workspace_snapshot_id
        .is_none()
        .then_some(source_event)
        .flatten()
        .map(
            |(event_id, recorded_at_stream_sequence)| WorkspaceMutationEvidence {
                event_id,
                source_event_type: "workspace_snapshot_incomplete".to_owned(),
                source_label: None,
                recovery_hint: None,
                scope_hash: scope.scope_hash.clone(),
                recorded_at_stream_sequence,
                from_workspace_snapshot_id: None,
                to_workspace_snapshot_id: None,
                tool_effect: ToolEffect::Unknown,
                unknown_dirty: true,
            },
        );
    let workspace_knowledge = if workspace_snapshot_id.is_some() {
        WorkspaceKnowledge::Clean(workspace_revision)
    } else {
        WorkspaceKnowledge::UnknownDirty
    };
    Ok(WorkspaceSnapshotBuild {
        manifest,
        workspace_snapshot_id,
        workspace_knowledge,
        unknown_dirty_evidence,
    })
}

impl WorkspaceSnapshotManifestV1 {
    pub fn workspace_snapshot_id(&self) -> Result<WorkspaceSnapshotId> {
        if self.entries.iter().any(|entry| !entry.is_complete()) {
            bail!("workspace snapshot manifest contains incomplete entries");
        }
        let mut normalized = self.clone();
        normalized
            .entries
            .sort_by(|left, right| left.normalized_path.cmp(&right.normalized_path));
        let value = serde_json::to_value(&normalized).map_err(|error| {
            anyhow!("failed to convert workspace snapshot manifest to json: {error}")
        })?;
        let bytes = canonical_json_bytes(&value)?;
        let digest = Sha256::digest(bytes);
        Ok(format!("sha256:jcs-v1:{digest:x}"))
    }
}

fn glob_set(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(
            Glob::new(pattern)
                .with_context(|| format!("invalid verification scope pattern {pattern:?}"))?,
        );
    }
    builder
        .build()
        .context("failed to build verification scope matcher")
}

fn effective_exclude_patterns(scope: &VerificationScope) -> Vec<String> {
    let mut patterns = scope.exclude.clone();
    for root in &scope.generated_roots {
        let root = root.to_string_lossy().replace('\\', "/");
        patterns.push(root.clone());
        patterns.push(format!("{root}/**"));
    }
    patterns
}

fn git_snapshot_paths(workspace_root: &Path, scope: &VerificationScope) -> Option<Vec<PathBuf>> {
    if !scope.tracked_files_only || !scope.include.is_empty() {
        return None;
    }
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|raw| !raw.is_empty())
        .filter_map(|raw| std::str::from_utf8(raw).ok())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    Some(paths)
}

fn collect_snapshot_entries_for_paths(
    workspace_root: &Path,
    paths: Vec<PathBuf>,
    include_set: &GlobSet,
    exclude_set: &GlobSet,
    max_file_bytes: u64,
    entries: &mut Vec<WorkspaceSnapshotEntry>,
) {
    for relative in paths {
        if is_excluded(&relative, exclude_set) || !is_included(&relative, include_set) {
            continue;
        }
        let path = workspace_root.join(&relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => {
                entries.push(snapshot_entry(
                    relative,
                    FileType::File,
                    SnapshotEntryState::Missing,
                    None,
                    None,
                    None,
                ));
                continue;
            }
        };
        entries.push(snapshot_entry_for_path(
            workspace_root,
            &path,
            relative,
            &metadata,
            max_file_bytes,
        ));
    }
}

fn collect_snapshot_entries(
    workspace_root: &Path,
    dir: &Path,
    include_set: &GlobSet,
    exclude_set: &GlobSet,
    max_file_bytes: u64,
    entries: &mut Vec<WorkspaceSnapshotEntry>,
) -> Result<()> {
    let mut children = fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .with_context(|| format!("failed to read directory entry in {}", dir.display()))?;
    children.sort_by_key(|entry| entry.path());
    for child in children {
        let path = child.path();
        let relative = normalized_relative_path(workspace_root, &path)?;
        if is_excluded(&relative, exclude_set) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                entries.push(snapshot_entry(
                    relative,
                    FileType::Other,
                    SnapshotEntryState::PermissionDenied,
                    None,
                    None,
                    None,
                ));
                tracing::debug!(path = %path.display(), "failed to stat verification snapshot entry: {error}");
                continue;
            }
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_snapshot_entries(
                workspace_root,
                &path,
                include_set,
                exclude_set,
                max_file_bytes,
                entries,
            )?;
            continue;
        }
        if !is_included(&relative, include_set) {
            continue;
        }
        entries.push(snapshot_entry_for_path(
            workspace_root,
            &path,
            relative,
            &metadata,
            max_file_bytes,
        ));
    }
    Ok(())
}

fn add_missing_literal_includes(
    workspace_root: &Path,
    scope: &VerificationScope,
    entries: &mut Vec<WorkspaceSnapshotEntry>,
) -> Result<()> {
    for include in &scope.include {
        if has_glob_meta(include) {
            continue;
        }
        let path = workspace_root.join(include);
        if path.exists() {
            continue;
        }
        entries.push(snapshot_entry(
            PathBuf::from(include),
            FileType::File,
            SnapshotEntryState::Missing,
            None,
            None,
            None,
        ));
    }
    Ok(())
}

pub(super) fn snapshot_entry_for_path(
    workspace_root: &Path,
    path: &Path,
    relative: PathBuf,
    metadata: &fs::Metadata,
    max_file_bytes: u64,
) -> WorkspaceSnapshotEntry {
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return snapshot_symlink_entry(workspace_root, path, relative);
    }
    if file_type.is_file() {
        if metadata.len() > max_file_bytes {
            let mut entry = snapshot_entry(
                relative,
                FileType::File,
                SnapshotEntryState::Unsupported,
                None,
                file_mode(metadata),
                None,
            );
            entry.file_metadata = Some(file_metadata_evidence(metadata));
            return entry;
        }
        return match fs::read(path) {
            Ok(bytes) => {
                let mut entry = snapshot_entry(
                    relative,
                    FileType::File,
                    SnapshotEntryState::Present,
                    Some(stable_bytes_hash(&bytes)),
                    file_mode(metadata),
                    None,
                );
                entry.file_metadata = Some(file_metadata_evidence(metadata));
                entry
            }
            Err(error) => {
                tracing::debug!(path = %path.display(), "failed to read verification snapshot file: {error}");
                let mut entry = snapshot_entry(
                    relative,
                    FileType::File,
                    SnapshotEntryState::PermissionDenied,
                    None,
                    file_mode(metadata),
                    None,
                );
                entry.file_metadata = Some(file_metadata_evidence(metadata));
                entry
            }
        };
    }
    let mut entry = snapshot_entry(
        relative,
        FileType::Other,
        SnapshotEntryState::Unsupported,
        None,
        file_mode(metadata),
        None,
    );
    entry.file_metadata = Some(file_metadata_evidence(metadata));
    entry
}

fn snapshot_symlink_entry(
    workspace_root: &Path,
    path: &Path,
    relative: PathBuf,
) -> WorkspaceSnapshotEntry {
    let symlink_target = fs::read_link(path).ok();
    match fs::canonicalize(path) {
        Ok(target) if target.starts_with(workspace_root) => snapshot_entry(
            relative,
            FileType::Symlink,
            SnapshotEntryState::Present,
            None,
            None,
            symlink_target,
        ),
        Ok(_) => snapshot_entry(
            relative,
            FileType::Symlink,
            SnapshotEntryState::External,
            None,
            None,
            symlink_target,
        ),
        Err(_) => snapshot_entry(
            relative,
            FileType::Symlink,
            SnapshotEntryState::Unsupported,
            None,
            None,
            symlink_target,
        ),
    }
}

fn snapshot_entry(
    normalized_path: PathBuf,
    file_type: FileType,
    state: SnapshotEntryState,
    content_hash: Option<String>,
    mode: Option<u32>,
    symlink_target: Option<PathBuf>,
) -> WorkspaceSnapshotEntry {
    WorkspaceSnapshotEntry {
        normalized_path,
        file_type,
        content_hash,
        mode,
        file_metadata: None,
        symlink_target,
        state,
    }
}

fn normalized_relative_path(workspace_root: &Path, path: &Path) -> Result<PathBuf> {
    let relative = path
        .strip_prefix(workspace_root)
        .with_context(|| format!("failed to relativize {}", path.display()))?;
    Ok(relative.components().collect())
}

fn is_included(relative: &Path, include_set: &GlobSet) -> bool {
    include_set.is_empty() || include_set.is_match(relative)
}

fn is_excluded(relative: &Path, exclude_set: &GlobSet) -> bool {
    exclude_set.is_match(relative)
}

fn has_glob_meta(pattern: &str) -> bool {
    pattern.contains('*') || pattern.contains('?') || pattern.contains('[')
}

fn stable_bytes_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!("sha256:{digest:x}")
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    Some(metadata.permissions().mode())
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> Option<u32> {
    None
}

fn file_metadata_evidence(metadata: &fs::Metadata) -> FileMetadataEvidence {
    let platform = file_metadata_platform();
    let readonly = metadata.permissions().readonly();
    let unix_mode = file_mode(metadata);
    FileMetadataEvidence {
        platform,
        readonly,
        unix_mode,
    }
}

#[cfg(windows)]
const FILE_METADATA_PLATFORM: FileMetadataPlatform = FileMetadataPlatform::Windows;
#[cfg(unix)]
const FILE_METADATA_PLATFORM: FileMetadataPlatform = FileMetadataPlatform::Unix;
#[cfg(not(any(unix, windows)))]
const FILE_METADATA_PLATFORM: FileMetadataPlatform = FileMetadataPlatform::Other;

fn file_metadata_platform() -> FileMetadataPlatform {
    FILE_METADATA_PLATFORM
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct WorkspaceSnapshotEntry {
    pub normalized_path: PathBuf,
    pub file_type: FileType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_metadata: Option<FileMetadataEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symlink_target: Option<PathBuf>,
    pub state: SnapshotEntryState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct FileMetadataEvidence {
    pub platform: FileMetadataPlatform,
    pub readonly: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_mode: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileMetadataPlatform {
    Unix,
    Windows,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotEntryState {
    Present,
    Missing,
    PermissionDenied,
    External,
    Unsupported,
}

impl SnapshotEntryState {
    pub fn is_clean(self) -> bool {
        matches!(self, Self::Present | Self::Missing)
    }
}

impl WorkspaceSnapshotEntry {
    pub fn is_complete(&self) -> bool {
        match (self.state, self.file_type) {
            (SnapshotEntryState::Present, FileType::File) => self.content_hash.is_some(),
            (SnapshotEntryState::Present, FileType::Symlink) => self.symlink_target.is_some(),
            (SnapshotEntryState::Present, FileType::Directory | FileType::Other) => true,
            (SnapshotEntryState::Missing, _) => true,
            (
                SnapshotEntryState::PermissionDenied
                | SnapshotEntryState::External
                | SnapshotEntryState::Unsupported,
                _,
            ) => false,
        }
    }
}
