use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{
    MutationArtifactId, OperationId, SnapshotCoverage, artifact_blob_matches,
    atomic_write_artifact, bytes_hash, file_modified_ms, harden_artifact_dir, harden_artifact_file,
    short_hash, sync_parent, unix_time_ms,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationArtifactLifecycleStatus {
    Deleted,
    Expired,
    Unavailable,
}

/// Durable payload recorded when mutation artifact content is removed or becomes unavailable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct MutationArtifactLifecycleRecorded {
    pub artifact_id: MutationArtifactId,
    pub status: MutationArtifactLifecycleStatus,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operation_ids: Vec<OperationId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_paths: Vec<PathBuf>,
}

pub(super) fn default_mutation_artifact_root(session_path: &Path) -> PathBuf {
    let Some(parent) = session_path.parent() else {
        return PathBuf::from(".sigil-state")
            .join("artifacts")
            .join("mutations");
    };
    let base = if parent.file_name().is_some_and(|name| name == "sessions") {
        let session_base = parent.parent().unwrap_or(parent);
        if session_base
            .file_name()
            .is_some_and(|name| name == ".sigil")
        {
            return default_user_state_mutation_artifact_root();
        }
        session_base
    } else {
        parent
    };
    base.join("artifacts").join("mutations")
}

fn default_user_state_mutation_artifact_root() -> PathBuf {
    user_state_root()
        .unwrap_or_else(|| PathBuf::from(".sigil-state"))
        .join("artifacts")
        .join("mutations")
}

fn user_state_root() -> Option<PathBuf> {
    if let Some(root) = env::var_os("SIGIL_STATE_HOME") {
        return Some(PathBuf::from(root));
    }
    match env::consts::OS {
        "macos" => env::var_os("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("sigil")
                .join("state")
        }),
        "windows" => env::var_os("LOCALAPPDATA")
            .map(|root| PathBuf::from(root).join("sigil").join("state"))
            .or_else(|| {
                env::var_os("USERPROFILE").map(|home| {
                    PathBuf::from(home)
                        .join("AppData")
                        .join("Local")
                        .join("sigil")
                        .join("state")
                })
            }),
        _ => env::var_os("XDG_STATE_HOME")
            .map(|root| PathBuf::from(root).join("sigil"))
            .or_else(|| {
                env::var_os("HOME").map(|home| {
                    PathBuf::from(home)
                        .join(".local")
                        .join("state")
                        .join("sigil")
                })
            }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) struct MutationArtifactMetadata {
    pub(super) artifact_id: MutationArtifactId,
    pub(super) content_hash: String,
    pub(super) size: u64,
    pub(super) workspace_id_hash: String,
    pub(super) operation_id: OperationId,
    pub(super) source_path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) created_at_ms: Option<u64>,
}

pub(super) fn snapshot_coverage_for_pre_mutation_content(
    artifact_root: &Path,
    workspace_id: &str,
    operation_id: &str,
    relative_path: &Path,
    absolute_path: &Path,
    before_hash: Option<&str>,
) -> Result<SnapshotCoverage> {
    let Some(before_hash) = before_hash else {
        return Ok(SnapshotCoverage::NoPriorContent);
    };
    if is_sensitive_snapshot_path(relative_path) {
        return Ok(SnapshotCoverage::SkippedSensitive);
    }
    let bytes = fs::read(absolute_path)
        .with_context(|| format!("failed to read {}", absolute_path.display()))?;
    let content_hash = bytes_hash(&bytes);
    if content_hash != before_hash {
        bail!(
            "pre-mutation artifact hash changed while capturing {}",
            absolute_path.display()
        );
    }
    let artifact_id = store_mutation_artifact(
        artifact_root,
        workspace_id,
        operation_id,
        relative_path,
        &bytes,
    )?;
    Ok(SnapshotCoverage::Captured(artifact_id))
}

fn store_mutation_artifact(
    artifact_root: &Path,
    workspace_id: &str,
    operation_id: &str,
    relative_path: &Path,
    bytes: &[u8],
) -> Result<MutationArtifactId> {
    let content_hash = bytes_hash(bytes);
    let workspace_id_hash = short_hash(workspace_id);
    let operation_id_hash = short_hash(operation_id);
    let digest = content_hash
        .strip_prefix("sha256:")
        .unwrap_or(content_hash.as_str())
        .to_owned();
    let artifact_id = format!("mutation-artifact:sha256:{digest}");
    let dir = artifact_root
        .join(&workspace_id_hash)
        .join(&operation_id_hash);
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    harden_artifact_dir(&dir)?;
    let blob_path = dir.join(format!("{digest}.blob"));
    if !artifact_blob_matches(&blob_path, &content_hash)? {
        atomic_write_artifact(&blob_path, bytes)?;
    }
    harden_artifact_file(&blob_path)?;
    let metadata = MutationArtifactMetadata {
        artifact_id: artifact_id.clone(),
        content_hash,
        size: bytes.len() as u64,
        workspace_id_hash,
        operation_id: operation_id.to_owned(),
        source_path: relative_path.to_path_buf(),
        created_at_ms: Some(unix_time_ms()),
    };
    let metadata_path = dir.join(format!("{digest}.json"));
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .context("failed to encode mutation artifact metadata")?;
    let mut metadata_file = File::create(&metadata_path)
        .with_context(|| format!("failed to create {}", metadata_path.display()))?;
    metadata_file
        .write_all(&metadata_bytes)
        .with_context(|| format!("failed to write {}", metadata_path.display()))?;
    metadata_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", metadata_path.display()))?;
    harden_artifact_file(&metadata_path)?;
    let dir_file = File::open(&dir).with_context(|| format!("failed to open {}", dir.display()))?;
    dir_file
        .sync_all()
        .with_context(|| format!("failed to sync {}", dir.display()))?;
    sync_parent(&dir)?;
    Ok(artifact_id)
}

pub(super) fn read_mutation_artifact_content(
    artifact_root: &Path,
    artifact_id: &MutationArtifactId,
) -> Result<Vec<u8>> {
    let located = locate_mutation_artifacts(artifact_root, artifact_id)?;
    for artifact in located {
        if !artifact.blob_available {
            continue;
        }
        let bytes = fs::read(&artifact.blob_path).with_context(|| {
            format!(
                "failed to read artifact blob {}",
                artifact.blob_path.display()
            )
        })?;
        let content_hash = bytes_hash(&bytes);
        if content_hash != artifact.metadata.content_hash {
            bail!(
                "mutation artifact content hash mismatch for {}",
                artifact.blob_path.display()
            );
        }
        return Ok(bytes);
    }
    bail!("mutation artifact not found: {artifact_id}")
}

#[derive(Debug)]
pub(super) struct LocatedMutationArtifact {
    pub(super) metadata: MutationArtifactMetadata,
    pub(super) metadata_path: PathBuf,
    pub(super) blob_path: PathBuf,
    pub(super) blob_available: bool,
}

#[derive(Debug)]
pub(super) struct MutationArtifactGroup {
    pub(super) artifact_id: MutationArtifactId,
    pub(super) size: u64,
    pub(super) created_at_ms: Option<u64>,
    pub(super) blob_available: bool,
    pub(super) workspace_id_hashes: Vec<String>,
    pub(super) operation_ids: Vec<OperationId>,
    pub(super) source_paths: Vec<PathBuf>,
}

pub(super) fn scan_mutation_artifact_groups(
    artifact_root: &Path,
) -> Result<Vec<MutationArtifactGroup>> {
    let mut by_id = BTreeMap::<MutationArtifactId, Vec<LocatedMutationArtifact>>::new();
    for artifact in scan_mutation_artifacts(artifact_root)? {
        by_id
            .entry(artifact.metadata.artifact_id.clone())
            .or_default()
            .push(artifact);
    }
    let mut groups = Vec::with_capacity(by_id.len());
    for (artifact_id, located) in by_id {
        let size = located.iter().fold(0_u64, |total, artifact| {
            total.saturating_add(artifact.metadata.size)
        });
        let created_at_ms = located
            .iter()
            .filter_map(|artifact| {
                artifact
                    .metadata
                    .created_at_ms
                    .or_else(|| file_modified_ms(&artifact.metadata_path))
            })
            .min();
        let blob_available = located.iter().any(|artifact| artifact.blob_available);
        let mut operation_ids = located
            .iter()
            .map(|artifact| artifact.metadata.operation_id.clone())
            .collect::<Vec<_>>();
        operation_ids.sort();
        operation_ids.dedup();
        let mut source_paths = located
            .iter()
            .map(|artifact| artifact.metadata.source_path.clone())
            .collect::<Vec<_>>();
        source_paths.sort();
        source_paths.dedup();
        let mut workspace_id_hashes = located
            .iter()
            .map(|artifact| artifact.metadata.workspace_id_hash.clone())
            .collect::<Vec<_>>();
        workspace_id_hashes.sort();
        workspace_id_hashes.dedup();
        groups.push(MutationArtifactGroup {
            artifact_id,
            size,
            created_at_ms,
            blob_available,
            workspace_id_hashes,
            operation_ids,
            source_paths,
        });
    }
    Ok(groups)
}

fn scan_mutation_artifacts(artifact_root: &Path) -> Result<Vec<LocatedMutationArtifact>> {
    let mut located = Vec::new();
    if !artifact_root.exists() {
        return Ok(located);
    }
    let mut pending = vec![artifact_root.to_path_buf()];
    let mut visited = BTreeSet::<PathBuf>::new();
    while let Some(dir) = pending.pop() {
        if !visited.insert(dir.clone()) {
            continue;
        }
        let entries =
            fs::read_dir(&dir).with_context(|| format!("failed to read {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let metadata = read_mutation_artifact_metadata(&path)?;
            let digest = mutation_artifact_digest(&metadata.artifact_id)?;
            let blob_path = path.with_file_name(format!("{digest}.blob"));
            let blob_available = artifact_blob_matches(&blob_path, &metadata.content_hash)?;
            located.push(LocatedMutationArtifact {
                metadata,
                metadata_path: path,
                blob_path,
                blob_available,
            });
        }
    }
    Ok(located)
}

pub(super) fn locate_mutation_artifacts(
    artifact_root: &Path,
    artifact_id: &MutationArtifactId,
) -> Result<Vec<LocatedMutationArtifact>> {
    let digest = mutation_artifact_digest(artifact_id)?;
    let metadata_name = format!("{digest}.json");
    let blob_name = format!("{digest}.blob");
    let mut located = Vec::new();
    let mut pending = vec![artifact_root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", dir.display()))?;
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
                continue;
            }
            if path.file_name().and_then(|name| name.to_str()) != Some(metadata_name.as_str()) {
                continue;
            }
            let metadata = read_mutation_artifact_metadata(&path)?;
            if metadata.artifact_id != *artifact_id {
                continue;
            }
            let blob_path = path.with_file_name(&blob_name);
            let blob_available = artifact_blob_matches(&blob_path, &metadata.content_hash)?;
            located.push(LocatedMutationArtifact {
                metadata,
                metadata_path: path,
                blob_path,
                blob_available,
            });
        }
    }
    Ok(located)
}

fn read_mutation_artifact_metadata(path: &Path) -> Result<MutationArtifactMetadata> {
    let metadata_bytes = fs::read(path)
        .with_context(|| format!("failed to read artifact metadata {}", path.display()))?;
    serde_json::from_slice(&metadata_bytes)
        .with_context(|| format!("failed to decode artifact metadata {}", path.display()))
}

fn mutation_artifact_digest(artifact_id: &MutationArtifactId) -> Result<&str> {
    artifact_id
        .strip_prefix("mutation-artifact:sha256:")
        .ok_or_else(|| anyhow!("unsupported mutation artifact id: {artifact_id}"))
}

fn is_sensitive_snapshot_path(path: &Path) -> bool {
    let components = path
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let Some(file_name) = components.last() else {
        return false;
    };
    const SENSITIVE_FILE_NAMES: &[&str] = &[
        ".env",
        ".netrc",
        ".npmrc",
        ".pypirc",
        ".yarnrc",
        "credentials",
        "credentials.json",
        "service-account.json",
        "service_account.json",
        "known_hosts",
        "config",
        "id_rsa",
        "id_dsa",
        "id_ecdsa",
        "id_ed25519",
    ];
    const SENSITIVE_NAME_PARTS: &[&str] = &[
        "api_key",
        "apikey",
        "auth",
        "credential",
        "oauth",
        "password",
        "private_key",
        "secret",
        "service-account",
        "service_account",
        "token",
    ];
    file_name == ".env"
        || file_name.starts_with(".env.")
        || SENSITIVE_FILE_NAMES.contains(&file_name.as_str())
        || file_name.ends_with(".pem")
        || file_name.ends_with(".key")
        || SENSITIVE_NAME_PARTS
            .iter()
            .any(|part| file_name.contains(part))
        || components
            .iter()
            .any(|component| matches!(component.as_str(), ".ssh" | ".aws" | ".azure" | ".gnupg"))
        || components
            .windows(2)
            .any(|pair| pair[0] == ".config" && pair[1] == "gcloud")
}
