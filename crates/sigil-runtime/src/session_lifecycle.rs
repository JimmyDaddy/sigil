use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    AssistantMessageKind, ControlEntry, ConversationForkProjection, ExternalProvenanceEntry,
    JsonlSessionStore, MessageRole, ModelMessage, SessionLogEntry, SessionRef,
    SessionStreamCompatibilityError, SessionStreamRecord, safe_persistence_text,
};

mod journal;

pub use journal::{
    LOCAL_SESSION_LIFECYCLE_JOURNAL_SCHEMA_VERSION, LocalSessionDeleteJournalBinding,
    LocalSessionExportJournalBinding, LocalSessionLifecycleEvent, LocalSessionLifecycleRecord,
};

use journal::LocalSessionLifecycleJournal;

pub const SESSION_EXPORT_SCHEMA_VERSION: u16 = 1;
pub const DEFAULT_SESSION_CATALOG_MAX_ENTRIES: usize = 4_096;
pub const DEFAULT_SESSION_CATALOG_MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_SESSION_CATALOG_MAX_TOTAL_VALIDATION_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_SESSION_EXPORT_MAX_MESSAGES: usize = 20_000;
pub const DEFAULT_SESSION_EXPORT_MAX_BYTES: usize = 32 * 1024 * 1024;
const SESSION_TITLE_MAX_BYTES: usize = 160;

/// Explicit resource limits for local session discovery and portable export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalSessionLifecycleLimits {
    pub max_catalog_entries: usize,
    pub max_stream_bytes: u64,
    pub max_total_validation_bytes: u64,
    pub max_export_messages: usize,
    pub max_export_bytes: usize,
}

impl Default for LocalSessionLifecycleLimits {
    fn default() -> Self {
        Self {
            max_catalog_entries: DEFAULT_SESSION_CATALOG_MAX_ENTRIES,
            max_stream_bytes: DEFAULT_SESSION_CATALOG_MAX_STREAM_BYTES,
            max_total_validation_bytes: DEFAULT_SESSION_CATALOG_MAX_TOTAL_VALIDATION_BYTES,
            max_export_messages: DEFAULT_SESSION_EXPORT_MAX_MESSAGES,
            max_export_bytes: DEFAULT_SESSION_EXPORT_MAX_BYTES,
        }
    }
}

/// Stable reason why a direct session file cannot be used by lifecycle operations.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalSessionCatalogState {
    Ready,
    Oversized,
    ScanBudgetExceeded,
    UnsupportedLegacy,
    Invalid,
}

/// Bounded metadata for one direct child of the configured session directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionCatalogEntry {
    pub session_ref: SessionRef,
    pub path: PathBuf,
    pub state: LocalSessionCatalogState,
    pub bytes: u64,
    pub modified_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub transcript_message_count: usize,
    pub finalized_turn_count: usize,
}

/// Deterministically ordered view of local V2 session files.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionCatalog {
    pub entries: Vec<LocalSessionCatalogEntry>,
    pub truncated_entry_count: usize,
}

/// Provider-neutral message retained in the user-facing export artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionExportMessageV1 {
    pub message_id: String,
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_kind: Option<AssistantMessageKind>,
}

/// Content-bound payload of one safe local session export.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionExportPayloadV1 {
    pub workspace_id: String,
    pub source_session_ref: SessionRef,
    pub source_session_id: String,
    pub source_content_sha256: String,
    pub exported_at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    pub messages: Vec<SessionExportMessageV1>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_provenance: Vec<ExternalProvenanceEntry>,
}

/// Portable JSON artifact. The digest binds the canonical serialized `payload` only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionExportV1 {
    pub schema_version: u16,
    pub payload: SessionExportPayloadV1,
    pub payload_sha256: String,
}

impl SessionExportV1 {
    /// Recomputes the artifact payload digest.
    ///
    /// # Errors
    ///
    /// Returns an error when the payload cannot be serialized.
    pub fn validate_digest(&self) -> Result<()> {
        if self.schema_version != SESSION_EXPORT_SCHEMA_VERSION {
            bail!("unsupported session export schema version");
        }
        let digest = digest_serializable(&self.payload)?;
        if digest != self.payload_sha256 {
            bail!("session export payload digest does not match");
        }
        Ok(())
    }
}

/// Successful atomic export receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionExportOutput {
    pub path: PathBuf,
    pub operation_id: String,
    pub source_session_id: String,
    pub payload_sha256: String,
    pub message_count: usize,
    pub journal_sequence: u64,
}

/// Exact read-only delete preview. Apply must revalidate every field and the digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionDeletePreview {
    pub source_path: PathBuf,
    pub source_session_ref: SessionRef,
    pub source_session_id: String,
    pub source_content_sha256: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
    pub preview_digest: String,
}

/// Successful audited session deletion receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionDeleteOutput {
    pub operation_id: String,
    pub source_session_ref: SessionRef,
    pub deleted_bytes: u64,
    pub journal_sequence: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalSessionLifecycleOperationKind {
    Export,
    Delete,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalSessionLifecycleRecoveryStatus {
    Completed,
    NotApplied,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionLifecycleRecoveryEntry {
    pub operation_id: String,
    pub kind: LocalSessionLifecycleOperationKind,
    pub status: LocalSessionLifecycleRecoveryStatus,
}

/// Workspace-bound local session lifecycle service.
#[derive(Debug, Clone)]
pub struct LocalSessionLifecycleService {
    workspace_id: String,
    session_dir: PathBuf,
    export_dir: PathBuf,
    lifecycle_journal_path: PathBuf,
    limits: LocalSessionLifecycleLimits,
}

impl LocalSessionLifecycleService {
    #[must_use]
    pub fn new(
        workspace_id: impl Into<String>,
        session_dir: impl Into<PathBuf>,
        export_dir: impl Into<PathBuf>,
    ) -> Self {
        let export_dir = export_dir.into();
        let lifecycle_journal_path = export_dir
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("session-lifecycle-v1.jsonl");
        Self {
            workspace_id: workspace_id.into(),
            session_dir: session_dir.into(),
            export_dir,
            lifecycle_journal_path,
            limits: LocalSessionLifecycleLimits::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: LocalSessionLifecycleLimits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn with_lifecycle_journal_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.lifecycle_journal_path = path.into();
        self
    }

    /// Reads and validates the workspace lifecycle hash chain.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal is oversized, busy, malformed, or tampered.
    pub fn lifecycle_records(&self) -> Result<Vec<LocalSessionLifecycleRecord>> {
        self.lifecycle_journal().read_records()
    }

    /// Projects incomplete lifecycle operations without retrying any side effect.
    ///
    /// # Errors
    ///
    /// Returns an error when the journal or a candidate source needed for recovery is unreadable.
    pub fn lifecycle_recovery(&self) -> Result<Vec<LocalSessionLifecycleRecoveryEntry>> {
        let records = self.lifecycle_records()?;
        let mut operations = BTreeMap::<String, Vec<LocalSessionLifecycleEvent>>::new();
        for record in records {
            operations
                .entry(record.operation_id)
                .or_default()
                .push(record.event);
        }
        operations
            .into_iter()
            .map(|(operation_id, events)| {
                let last = events
                    .last()
                    .ok_or_else(|| anyhow!("lifecycle operation has no events"))?;
                let (kind, status) = match last {
                    LocalSessionLifecycleEvent::ExportCompleted(_) => (
                        LocalSessionLifecycleOperationKind::Export,
                        LocalSessionLifecycleRecoveryStatus::Completed,
                    ),
                    LocalSessionLifecycleEvent::ExportPlanned(_) => (
                        LocalSessionLifecycleOperationKind::Export,
                        LocalSessionLifecycleRecoveryStatus::Uncertain,
                    ),
                    LocalSessionLifecycleEvent::DeleteCompleted(_) => (
                        LocalSessionLifecycleOperationKind::Delete,
                        LocalSessionLifecycleRecoveryStatus::Completed,
                    ),
                    LocalSessionLifecycleEvent::DeletePlanned(binding) => (
                        LocalSessionLifecycleOperationKind::Delete,
                        self.recover_incomplete_delete(binding),
                    ),
                };
                Ok(LocalSessionLifecycleRecoveryEntry {
                    operation_id,
                    kind,
                    status,
                })
            })
            .collect()
    }

    /// Scans direct JSONL children in deterministic modified-time order.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured directory exists but cannot be canonicalized/read.
    pub fn catalog(&self) -> Result<LocalSessionCatalog> {
        if !self.session_dir.exists() {
            return Ok(LocalSessionCatalog::default());
        }
        if fs::symlink_metadata(&self.session_dir)
            .with_context(|| {
                format!(
                    "failed to inspect session directory {}",
                    self.session_dir.display()
                )
            })?
            .file_type()
            .is_symlink()
        {
            bail!("configured session directory must not be a symlink");
        }
        let session_dir = fs::canonicalize(&self.session_dir).with_context(|| {
            format!(
                "failed to canonicalize session directory {}",
                self.session_dir.display()
            )
        })?;
        let mut candidates = direct_jsonl_candidates(&session_dir)?;
        if let Ok(journal_path) = fs::canonicalize(&self.lifecycle_journal_path) {
            candidates.retain(|candidate| candidate.path != journal_path);
        }
        candidates.sort_by(|left, right| {
            right
                .modified_at_unix_ms
                .cmp(&left.modified_at_unix_ms)
                .then_with(|| left.path.cmp(&right.path))
        });
        let truncated_entry_count = candidates
            .len()
            .saturating_sub(self.limits.max_catalog_entries);
        candidates.truncate(self.limits.max_catalog_entries);

        let mut validated_bytes = 0_u64;
        let mut entries = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let state = if candidate.symlink_or_non_file {
                LocalSessionCatalogState::Invalid
            } else if candidate.bytes > self.limits.max_stream_bytes {
                LocalSessionCatalogState::Oversized
            } else if validated_bytes.saturating_add(candidate.bytes)
                > self.limits.max_total_validation_bytes
            {
                LocalSessionCatalogState::ScanBudgetExceeded
            } else {
                validated_bytes = validated_bytes.saturating_add(candidate.bytes);
                LocalSessionCatalogState::Ready
            };
            entries.push(self.catalog_entry(candidate, state));
        }
        Ok(LocalSessionCatalog {
            entries,
            truncated_entry_count,
        })
    }

    /// Writes a content-bound safe transcript artifact without overwriting an existing path.
    ///
    /// `destination=None` allocates a unique file under the service export directory.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-ready source, source drift, unsafe destination, export limit, or
    /// any failure before the create-new artifact is fully synced.
    pub fn export_session(
        &self,
        source_path: &Path,
        destination: Option<&Path>,
        exported_at_unix_ms: u64,
    ) -> Result<SessionExportOutput> {
        let source = self.resolve_ready_source(source_path)?;
        let before_hash = hash_file_bounded(&source.path, self.limits.max_stream_bytes)?;
        let records = JsonlSessionStore::read_event_records(&source.path)?;
        let projection = project_records(&records)?;
        let after_hash = hash_file_bounded(&source.path, self.limits.max_stream_bytes)?;
        if before_hash != after_hash {
            bail!("source session changed while export was being prepared");
        }
        let source_session_id = projection
            .session_id
            .clone()
            .ok_or_else(|| anyhow!("source session has no durable identity"))?;
        let messages = export_messages(&projection.messages, self.limits.max_export_messages)?;
        validate_export_provenance(&messages, &projection.external_provenance)?;
        let payload = SessionExportPayloadV1 {
            workspace_id: self.workspace_id.clone(),
            source_session_ref: source.session_ref,
            source_session_id: source_session_id.clone(),
            source_content_sha256: before_hash,
            exported_at_unix_ms,
            provider_name: projection.provider_name,
            model_name: projection.model_name,
            messages,
            external_provenance: projection.external_provenance,
        };
        let payload_sha256 = digest_serializable(&payload)?;
        let artifact = SessionExportV1 {
            schema_version: SESSION_EXPORT_SCHEMA_VERSION,
            payload,
            payload_sha256: payload_sha256.clone(),
        };
        let mut bytes = serde_json::to_vec_pretty(&artifact)
            .context("failed to serialize safe session export")?;
        bytes.push(b'\n');
        if bytes.len() > self.limits.max_export_bytes {
            bail!("safe session export exceeds configured artifact byte limit");
        }
        let output_path = match destination {
            Some(path) => path.to_path_buf(),
            None => self.allocate_export_path(&source.path, exported_at_unix_ms)?,
        };
        let canonical_destination = canonical_destination_candidate(&output_path)?;
        let binding = LocalSessionExportJournalBinding {
            source_session_ref: artifact.payload.source_session_ref.clone(),
            source_session_id: source_session_id.clone(),
            source_content_sha256: artifact.payload.source_content_sha256.clone(),
            destination_file_name: canonical_destination
                .file_name()
                .and_then(|value| value.to_str())
                .map(safe_persistence_text)
                .map(|value| truncate_utf8(&value, SESSION_TITLE_MAX_BYTES))
                .unwrap_or_else(|| "session-export.json".to_owned()),
            destination_path_sha256: digest_serializable(&canonical_destination.to_string_lossy())?,
            artifact_payload_sha256: payload_sha256.clone(),
            message_count: artifact.payload.messages.len(),
        };
        let operation_id = format!("session-export:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            exported_at_unix_ms,
            LocalSessionLifecycleEvent::ExportPlanned(binding.clone()),
        )?;
        write_atomic_create_new(&output_path, &bytes)?;
        let completed = self.lifecycle_journal().append(
            &operation_id,
            exported_at_unix_ms,
            LocalSessionLifecycleEvent::ExportCompleted(binding),
        )?;
        Ok(SessionExportOutput {
            path: output_path,
            operation_id,
            source_session_id,
            payload_sha256,
            message_count: artifact.payload.messages.len(),
            journal_sequence: completed.sequence,
        })
    }

    /// Builds a read-only, content-bound preview for deleting one inactive local session.
    ///
    /// # Errors
    ///
    /// Returns an error for current/protected, invalid, unsupported, symlinked, or drifting
    /// sources.
    pub fn preview_delete(
        &self,
        source_path: &Path,
        protected_paths: &[PathBuf],
    ) -> Result<SessionDeletePreview> {
        let source = self.resolve_ready_source(source_path)?;
        ensure_not_protected(&source.path, protected_paths)?;
        let source_content_sha256 = hash_file_bounded(&source.path, self.limits.max_stream_bytes)?;
        let metadata = fs::metadata(&source.path)
            .with_context(|| format!("failed to inspect {}", source.path.display()))?;
        let source_bytes = metadata.len();
        let source_modified_at_unix_ms = modified_at_unix_ms(&metadata);
        if source_bytes != source.bytes || source_modified_at_unix_ms != source.modified_at_unix_ms
        {
            bail!("source session changed while delete preview was being prepared");
        }
        let source_session_id = source
            .session_id
            .ok_or_else(|| anyhow!("source session has no durable identity"))?;
        let preview_digest = delete_preview_digest(
            &self.workspace_id,
            &source.session_ref,
            &source_session_id,
            &source_content_sha256,
            source_bytes,
            source_modified_at_unix_ms,
        )?;
        Ok(SessionDeletePreview {
            source_path: source.path,
            source_session_ref: source.session_ref,
            source_session_id,
            source_content_sha256,
            source_bytes,
            source_modified_at_unix_ms,
            preview_digest,
        })
    }

    /// Applies one exact delete preview after acquiring the source writer lease.
    ///
    /// # Errors
    ///
    /// Returns before deleting when the preview is stale, the source is protected, or another
    /// process owns the session writer lease. Once a planned record is durable, later failures are
    /// recoverable as uncertain rather than silently retried.
    pub fn apply_delete(
        &self,
        preview: &SessionDeletePreview,
        protected_paths: &[PathBuf],
        applied_at_unix_ms: u64,
    ) -> Result<SessionDeleteOutput> {
        validate_delete_preview(&self.workspace_id, preview)?;
        reject_source_symlink_and_escape(
            &self.session_dir,
            &preview.source_path,
            &preview.source_session_ref,
        )?;
        ensure_not_protected(&preview.source_path, protected_paths)?;
        let lease = acquire_session_writer_lease(&preview.source_path)?;
        let metadata = fs::metadata(&preview.source_path)
            .with_context(|| format!("failed to inspect {}", preview.source_path.display()))?;
        let observed_hash = hash_file_bounded(&preview.source_path, self.limits.max_stream_bytes)?;
        if metadata.len() != preview.source_bytes
            || modified_at_unix_ms(&metadata) != preview.source_modified_at_unix_ms
            || observed_hash != preview.source_content_sha256
        {
            bail!("source session changed after delete preview");
        }
        let binding = LocalSessionDeleteJournalBinding {
            source_session_ref: preview.source_session_ref.clone(),
            source_session_id: preview.source_session_id.clone(),
            source_content_sha256: preview.source_content_sha256.clone(),
            source_bytes: preview.source_bytes,
            source_modified_at_unix_ms: preview.source_modified_at_unix_ms,
            preview_digest: preview.preview_digest.clone(),
        };
        let operation_id = format!("session-delete:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            applied_at_unix_ms,
            LocalSessionLifecycleEvent::DeletePlanned(binding.clone()),
        )?;
        fs::remove_file(&preview.source_path)
            .with_context(|| format!("failed to delete {}", preview.source_path.display()))?;
        let session_parent = preview
            .source_path
            .parent()
            .ok_or_else(|| anyhow!("source session has no parent directory"))?;
        sync_directory(session_parent)?;
        drop(lease);
        let completed = self.lifecycle_journal().append(
            &operation_id,
            applied_at_unix_ms,
            LocalSessionLifecycleEvent::DeleteCompleted(binding),
        )?;
        Ok(SessionDeleteOutput {
            operation_id,
            source_session_ref: preview.source_session_ref.clone(),
            deleted_bytes: preview.source_bytes,
            journal_sequence: completed.sequence,
        })
    }

    fn catalog_entry(
        &self,
        candidate: SessionCandidate,
        initial_state: LocalSessionCatalogState,
    ) -> LocalSessionCatalogEntry {
        let session_ref = candidate.session_ref;
        let mut entry = LocalSessionCatalogEntry {
            session_ref,
            path: candidate.path.clone(),
            state: initial_state,
            bytes: candidate.bytes,
            modified_at_unix_ms: candidate.modified_at_unix_ms,
            session_id: None,
            provider_name: None,
            model_name: None,
            title: None,
            transcript_message_count: 0,
            finalized_turn_count: 0,
        };
        if initial_state != LocalSessionCatalogState::Ready {
            return entry;
        }
        let records = match JsonlSessionStore::read_event_records(&candidate.path) {
            Ok(records) => records,
            Err(error) => {
                entry.state = if error
                    .downcast_ref::<SessionStreamCompatibilityError>()
                    .is_some()
                {
                    LocalSessionCatalogState::UnsupportedLegacy
                } else {
                    LocalSessionCatalogState::Invalid
                };
                return entry;
            }
        };
        let projection = match project_records(&records) {
            Ok(projection) if projection.session_id.is_some() => projection,
            Ok(_) | Err(_) => {
                entry.state = LocalSessionCatalogState::Invalid;
                return entry;
            }
        };
        entry.session_id = projection.session_id;
        entry.provider_name = projection.provider_name;
        entry.model_name = projection.model_name;
        entry.title = projection.title;
        entry.transcript_message_count = projection.messages.len();
        entry.finalized_turn_count = projection.finalized_turn_count;
        entry
    }

    fn resolve_ready_source(&self, source_path: &Path) -> Result<LocalSessionCatalogEntry> {
        if fs::symlink_metadata(source_path)
            .with_context(|| format!("failed to inspect {}", source_path.display()))?
            .file_type()
            .is_symlink()
        {
            bail!("source session must not be a symlink");
        }
        let catalog = self.catalog()?;
        let canonical_source = fs::canonicalize(source_path)
            .with_context(|| format!("failed to canonicalize {}", source_path.display()))?;
        let entry = catalog
            .entries
            .into_iter()
            .find(|entry| entry.path == canonical_source)
            .ok_or_else(|| anyhow!("source is not a cataloged direct session child"))?;
        if entry.state != LocalSessionCatalogState::Ready {
            bail!("source session is not ready for lifecycle operations");
        }
        Ok(entry)
    }

    fn allocate_export_path(
        &self,
        source_path: &Path,
        exported_at_unix_ms: u64,
    ) -> Result<PathBuf> {
        if self.export_dir.exists() {
            let metadata = fs::symlink_metadata(&self.export_dir).with_context(|| {
                format!(
                    "failed to inspect export directory {}",
                    self.export_dir.display()
                )
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("configured export directory must be a real directory");
            }
        } else {
            fs::create_dir_all(&self.export_dir).with_context(|| {
                format!(
                    "failed to create export directory {}",
                    self.export_dir.display()
                )
            })?;
        }
        let stem = source_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("session");
        for _ in 0..100 {
            let path = self.export_dir.join(format!(
                "{stem}-{exported_at_unix_ms}-{}.json",
                uuid::Uuid::new_v4().simple()
            ));
            if !path.exists() {
                return Ok(path);
            }
        }
        bail!("failed to allocate a unique session export path")
    }

    fn lifecycle_journal(&self) -> LocalSessionLifecycleJournal {
        LocalSessionLifecycleJournal::new(self.lifecycle_journal_path.clone())
    }

    fn recover_incomplete_delete(
        &self,
        binding: &LocalSessionDeleteJournalBinding,
    ) -> LocalSessionLifecycleRecoveryStatus {
        let path = binding.source_session_ref.resolve(&self.session_dir);
        if !path.exists() {
            return LocalSessionLifecycleRecoveryStatus::Uncertain;
        }
        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(true)
        {
            return LocalSessionLifecycleRecoveryStatus::Uncertain;
        }
        match hash_file_bounded(&path, self.limits.max_stream_bytes) {
            Ok(hash) if hash == binding.source_content_sha256 => {
                LocalSessionLifecycleRecoveryStatus::NotApplied
            }
            Ok(_) | Err(_) => LocalSessionLifecycleRecoveryStatus::Uncertain,
        }
    }
}

#[derive(Debug)]
struct SessionCandidate {
    session_ref: SessionRef,
    path: PathBuf,
    bytes: u64,
    modified_at_unix_ms: u64,
    symlink_or_non_file: bool,
}

fn direct_jsonl_candidates(session_dir: &Path) -> Result<Vec<SessionCandidate>> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(session_dir)
        .with_context(|| format!("failed to read session directory {}", session_dir.display()))?
    {
        let entry = entry.context("failed to read session directory entry")?;
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(file_name) = path.file_name() else {
            continue;
        };
        let Ok(session_ref) = SessionRef::new_relative(file_name) else {
            continue;
        };
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        let symlink_or_non_file = metadata.file_type().is_symlink() || !metadata.is_file();
        let canonical_path = if symlink_or_non_file {
            path
        } else {
            fs::canonicalize(&path)
                .with_context(|| format!("failed to canonicalize {}", path.display()))?
        };
        candidates.push(SessionCandidate {
            session_ref,
            path: canonical_path,
            bytes: metadata.len(),
            modified_at_unix_ms: modified_at_unix_ms(&metadata),
            symlink_or_non_file,
        });
    }
    Ok(candidates)
}

#[derive(Debug, Default)]
struct SessionRecordProjection {
    session_id: Option<String>,
    provider_name: Option<String>,
    model_name: Option<String>,
    title: Option<String>,
    messages: Vec<ModelMessage>,
    external_provenance: Vec<ExternalProvenanceEntry>,
    finalized_turn_count: usize,
}

fn project_records(records: &[SessionStreamRecord]) -> Result<SessionRecordProjection> {
    let mut projection = SessionRecordProjection {
        session_id: records.first().map(|record| record.session_id().to_owned()),
        finalized_turn_count: ConversationForkProjection::from_records(records)?
            .points
            .len(),
        ..SessionRecordProjection::default()
    };
    let mut messages_by_id = BTreeMap::new();
    for record in records {
        if projection
            .session_id
            .as_deref()
            .is_some_and(|session_id| session_id != record.session_id())
        {
            bail!("session stream contains multiple durable session identities");
        }
        let Some(entry) = session_entry(record)? else {
            continue;
        };
        match entry {
            SessionLogEntry::User(message)
            | SessionLogEntry::Assistant(message)
            | SessionLogEntry::ToolResult(message) => {
                if projection.title.is_none() && message.role == MessageRole::User {
                    projection.title = message
                        .content
                        .as_deref()
                        .map(safe_persistence_text)
                        .map(|title| truncate_utf8(&title, SESSION_TITLE_MAX_BYTES))
                        .filter(|title| !title.trim().is_empty());
                }
                messages_by_id.insert(message.id.clone(), message.clone());
                projection.messages.push(message);
            }
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name,
                model_name,
            }) => {
                projection.provider_name.get_or_insert(provider_name);
                projection.model_name.get_or_insert(model_name);
            }
            SessionLogEntry::Control(ControlEntry::ExternalProvenance(provenance)) => {
                projection.external_provenance.push(provenance);
            }
            SessionLogEntry::Control(_) => {}
        }
    }
    for provenance in &projection.external_provenance {
        let message = messages_by_id
            .get(&provenance.message_id)
            .ok_or_else(|| anyhow!("external provenance references an unknown message"))?;
        provenance.validate_against_message(message)?;
    }
    Ok(projection)
}

fn session_entry(record: &SessionStreamRecord) -> Result<Option<SessionLogEntry>> {
    record
        .stored_event()
        .payload
        .get("session_log_entry")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("failed to decode session lifecycle entry")
}

fn export_messages(messages: &[ModelMessage], limit: usize) -> Result<Vec<SessionExportMessageV1>> {
    if messages.len() > limit {
        bail!("session transcript exceeds configured export message limit");
    }
    Ok(messages
        .iter()
        .map(|message| SessionExportMessageV1 {
            message_id: message.id.clone(),
            role: message.role.clone(),
            content: message.content.as_deref().map(safe_persistence_text),
            assistant_kind: message.assistant_kind,
        })
        .collect())
}

fn validate_export_provenance(
    messages: &[SessionExportMessageV1],
    provenance_entries: &[ExternalProvenanceEntry],
) -> Result<()> {
    let messages = messages
        .iter()
        .map(|message| {
            (
                message.message_id.clone(),
                ModelMessage {
                    id: message.message_id.clone(),
                    role: message.role.clone(),
                    content: message.content.clone(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                    assistant_kind: message.assistant_kind,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    for provenance in provenance_entries {
        let message = messages
            .get(&provenance.message_id)
            .ok_or_else(|| anyhow!("external provenance references an omitted export message"))?;
        provenance.validate_against_message(message)?;
    }
    Ok(())
}

fn delete_preview_digest(
    workspace_id: &str,
    source_session_ref: &SessionRef,
    source_session_id: &str,
    source_content_sha256: &str,
    source_bytes: u64,
    source_modified_at_unix_ms: u64,
) -> Result<String> {
    digest_serializable(&(
        workspace_id,
        source_session_ref,
        source_session_id,
        source_content_sha256,
        source_bytes,
        source_modified_at_unix_ms,
    ))
}

fn validate_delete_preview(workspace_id: &str, preview: &SessionDeletePreview) -> Result<()> {
    let expected = delete_preview_digest(
        workspace_id,
        &preview.source_session_ref,
        &preview.source_session_id,
        &preview.source_content_sha256,
        preview.source_bytes,
        preview.source_modified_at_unix_ms,
    )?;
    if expected != preview.preview_digest {
        bail!("session delete preview digest does not match");
    }
    Ok(())
}

fn ensure_not_protected(source_path: &Path, protected_paths: &[PathBuf]) -> Result<()> {
    let source = fs::canonicalize(source_path)
        .with_context(|| format!("failed to canonicalize {}", source_path.display()))?;
    for protected in protected_paths {
        if fs::canonicalize(protected).ok().as_deref() == Some(source.as_path()) {
            bail!("current or protected session cannot be deleted");
        }
    }
    Ok(())
}

fn reject_source_symlink_and_escape(
    session_dir: &Path,
    source_path: &Path,
    source_session_ref: &SessionRef,
) -> Result<()> {
    if fs::symlink_metadata(source_path)
        .with_context(|| format!("failed to inspect {}", source_path.display()))?
        .file_type()
        .is_symlink()
    {
        bail!("source session must not be a symlink");
    }
    if fs::symlink_metadata(session_dir)
        .with_context(|| format!("failed to inspect {}", session_dir.display()))?
        .file_type()
        .is_symlink()
    {
        bail!("configured session directory must not be a symlink");
    }
    let canonical_dir = fs::canonicalize(session_dir)
        .with_context(|| format!("failed to canonicalize {}", session_dir.display()))?;
    let canonical_source = fs::canonicalize(source_path)
        .with_context(|| format!("failed to canonicalize {}", source_path.display()))?;
    if canonical_source.parent() != Some(canonical_dir.as_path()) {
        bail!("source session is not a direct child of the configured directory");
    }
    let referenced = source_session_ref.resolve(&canonical_dir);
    if fs::canonicalize(&referenced).ok().as_deref() != Some(canonical_source.as_path()) {
        bail!("source session reference does not match the delete target");
    }
    Ok(())
}

fn acquire_session_writer_lease(source_path: &Path) -> Result<File> {
    let file_name = source_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("source session file name is invalid"))?;
    let lease_path = source_path.with_file_name(format!("{file_name}.writer-lock"));
    if lease_path.exists()
        && fs::symlink_metadata(&lease_path)
            .with_context(|| format!("failed to inspect {}", lease_path.display()))?
            .file_type()
            .is_symlink()
    {
        bail!("session writer lease must not be a symlink");
    }
    let lease = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lease_path)
        .with_context(|| format!("failed to open {}", lease_path.display()))?;
    lease.try_lock().with_context(|| {
        format!(
            "session is active or its writer lease is busy: {}",
            source_path.display()
        )
    })?;
    Ok(lease)
}

fn hash_file_bounded(path: &Path, max_bytes: u64) -> Result<String> {
    let metadata =
        fs::metadata(path).with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.len() > max_bytes {
        bail!("session stream exceeds configured lifecycle byte limit");
    }
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut observed = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        observed = observed.saturating_add(read as u64);
        if observed > max_bytes {
            bail!("session stream grew beyond configured lifecycle byte limit");
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn digest_serializable(value: &impl Serialize) -> Result<String> {
    let bytes = serde_json::to_vec(value).context("failed to serialize digest payload")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn write_atomic_create_new(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        bail!("session export destination already exists");
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("session export destination has no parent directory"))?;
    let parent_metadata = fs::symlink_metadata(parent).with_context(|| {
        format!(
            "failed to inspect session export directory {}",
            parent.display()
        )
    })?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        bail!("session export destination parent must be a real directory");
    }
    let canonical_parent = fs::canonicalize(parent).with_context(|| {
        format!(
            "failed to canonicalize session export directory {}",
            parent.display()
        )
    })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("session export destination has no file name"))?;
    let destination = canonical_parent.join(file_name);
    if fs::symlink_metadata(&destination).is_ok() {
        bail!("session export destination already exists");
    }
    let temporary = canonical_parent.join(format!(
        ".{}.{}.tmp",
        file_name.to_string_lossy(),
        uuid::Uuid::new_v4().simple()
    ));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", temporary.display()))?;
        fs::hard_link(&temporary, &destination).with_context(|| {
            format!(
                "failed to atomically create session export {}",
                destination.display()
            )
        })?;
        let _ = fs::remove_file(&temporary);
        sync_directory(&canonical_parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn canonical_destination_candidate(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("session export destination has no parent directory"))?;
    let metadata = fs::symlink_metadata(parent).with_context(|| {
        format!(
            "failed to inspect session export directory {}",
            parent.display()
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("session export destination parent must be a real directory");
    }
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("session export destination has no file name"))?;
    let destination = fs::canonicalize(parent)
        .with_context(|| format!("failed to canonicalize {}", parent.display()))?
        .join(file_name);
    if fs::symlink_metadata(&destination).is_ok() {
        bail!("session export destination already exists");
    }
    Ok(destination)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

fn modified_at_unix_ms(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", &value[..end])
}

#[cfg(test)]
#[path = "tests/session_lifecycle_tests.rs"]
mod tests;
