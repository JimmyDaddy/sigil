use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::session::ToolArtifactDescriptorV1;
use sigil_kernel::{
    AssistantMessageKind, ControlEntry, ConversationForkOutput, ConversationForkProjection,
    ConversationForked, ConversationTurnForkRequest, DurableEventType, ExternalProvenanceEntry,
    JsonlSessionStore, MessageRole, ModelMessage, RootConfig, SessionLogEntry, SessionRef,
    SessionStreamRecord, ToolArtifactBindingV1, ToolArtifactGcReportV1, ToolArtifactGcRootsV1,
    ToolArtifactStore, fork_conversation_at_turn, safe_persistence_text,
};
use thiserror::Error as ThisError;

mod journal;
mod projection;
mod retention;

pub use journal::{
    LOCAL_SESSION_LIFECYCLE_JOURNAL_SCHEMA_VERSION, LocalSessionArtifactGcJournalBinding,
    LocalSessionDeleteJournalBinding, LocalSessionDisplayNameJournalBinding,
    LocalSessionExportJournalBinding, LocalSessionGeneratedTitleJournalBinding,
    LocalSessionLifecycleEvent, LocalSessionLifecycleRecord, LocalSessionPinJournalBinding,
    LocalSessionRetentionJournalBinding,
};
pub use projection::{
    DEFAULT_SESSION_CATALOG_PAGE_SIZE, MAX_SESSION_CATALOG_PAGE_SIZE,
    SESSION_CATALOG_APPLICATION_ID, SESSION_CATALOG_SCHEMA_VERSION,
    SessionCatalogInvalidSourceDeleteReceipt, SessionCatalogMutationReceipt,
    SessionCatalogProjectionEntry, SessionCatalogProjectionError, SessionCatalogProjectionPage,
    SessionCatalogProjectionQuery, SessionCatalogProjectionRebuildReport,
    SessionCatalogProjectionReconcileReport, SessionCatalogProjectionRecoveryReport,
    SessionCatalogProjectionService, SessionCatalogQuarantineReceipt,
};

use journal::LocalSessionLifecycleJournal;
pub use retention::{
    SessionRetentionCandidate, SessionRetentionOutput, SessionRetentionPolicy,
    SessionRetentionPreview, SessionRetentionReason,
};

pub const SESSION_EXPORT_SCHEMA_VERSION: u16 = 2;
pub const DEFAULT_SESSION_CATALOG_MAX_ENTRIES: usize = 4_096;
pub const DEFAULT_SESSION_CATALOG_MAX_STREAM_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_SESSION_CATALOG_MAX_TOTAL_VALIDATION_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_SESSION_EXPORT_MAX_MESSAGES: usize = 20_000;
pub const DEFAULT_SESSION_EXPORT_MAX_BYTES: usize = 32 * 1024 * 1024;
pub const SESSION_DELETE_TOMBSTONE_GRACE_MS: u64 = 24 * 60 * 60 * 1_000;
const SESSION_TITLE_MAX_BYTES: usize = 160;
const SESSION_RESOURCE_MAX_BYTES: u64 = 512 * 1024 * 1024;
const SESSION_RESOURCE_MAX_ENTRIES: usize = 200_000;

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
    Invalid,
}

impl LocalSessionCatalogState {
    /// Whether an exact non-ready source may be cleaned up without trusting a durable session
    /// identity.
    #[must_use]
    pub const fn permits_source_cleanup(self) -> bool {
        !matches!(self, Self::Ready)
    }
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
    pub pinned: bool,
}

/// Deterministically ordered view of local V2 session files.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionCatalog {
    pub entries: Vec<LocalSessionCatalogEntry>,
    pub truncated_entry_count: usize,
}

/// Canonical durable source proven safe for reopening in the current workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSessionReopenBinding {
    pub session_ref: SessionRef,
    pub session_id: String,
    pub session_log_path: PathBuf,
}

/// Stable failure direction for a desktop/app-server session reopen request.
#[derive(Debug, ThisError)]
pub enum LocalSessionReopenError {
    #[error("durable session was not found in the current workspace")]
    NotFound,
    #[error("durable session is not ready for reopen: {state:?}")]
    NotReady { state: LocalSessionCatalogState },
    #[error("durable session identity changed since it was listed")]
    IdentityChanged,
    #[error("durable session catalog is unavailable")]
    CatalogUnavailable {
        #[source]
        source: anyhow::Error,
    },
}

/// Stable failures for an exact local session rename or delete request.
#[derive(Debug, ThisError)]
pub enum LocalSessionMutationError {
    #[error("local session mutation request is invalid")]
    InvalidRequest,
    #[error("local session was not found")]
    NotFound,
    #[error("local session is not ready for mutation")]
    NotReady,
    #[error("local session identity changed")]
    IdentityChanged,
    #[error("pinned local session cannot be deleted")]
    Pinned,
    #[error("local session mutation is unavailable")]
    Unavailable {
        #[source]
        source: anyhow::Error,
    },
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_attachments: Vec<sigil_kernel::ImageAttachment>,
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
    pub tool_artifacts: SessionExportToolArtifactsV1,
}

/// Explicit policy selected for raw tool-output artifacts in a portable export.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionArtifactExportModeV1 {
    IncludeArtifacts,
    BoundedTranscript,
    RejectIfIncomplete,
}

/// Truthful completeness of the tool-output portion of a session export.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionArtifactExportCompletenessV1 {
    Complete,
    Incomplete,
}

/// One exported immutable artifact. Bodies are base64 and present only in include mode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionExportToolArtifactV1 {
    pub descriptor: ToolArtifactDescriptorV1,
    pub body_base64: String,
}

/// Explicit tool-output manifest retained in every export.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionExportToolArtifactsV1 {
    pub mode: SessionArtifactExportModeV1,
    pub completeness: SessionArtifactExportCompletenessV1,
    pub published_artifact_count: usize,
    pub included_artifact_count: usize,
    pub omitted_artifact_count: usize,
    pub unavailable_tool_result_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<SessionExportToolArtifactV1>,
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
    pub artifact_completeness: SessionArtifactExportCompletenessV1,
    pub included_artifact_count: usize,
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
    pub resource_tree_sha256: String,
    pub resource_bytes: u64,
    pub preview_digest: String,
}

/// Successful audited session deletion receipt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionDeleteOutput {
    pub operation_id: String,
    pub source_session_ref: SessionRef,
    pub deleted_bytes: u64,
    pub tombstoned_resource_bytes: u64,
    pub tombstone_id: String,
    pub journal_sequence: u64,
}

/// Result of unlinking delete tombstones whose mandatory grace has elapsed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct SessionTombstonePruneOutput {
    pub removed_tombstones: usize,
    pub removed_bytes: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalSessionLifecycleOperationKind {
    Export,
    Delete,
    Retention,
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
            if matches!(
                record.event,
                LocalSessionLifecycleEvent::PinChanged(_)
                    | LocalSessionLifecycleEvent::DisplayNameChanged(_)
                    | LocalSessionLifecycleEvent::GeneratedTitleChanged(_)
                    | LocalSessionLifecycleEvent::ArtifactGcCompleted(_)
            ) {
                continue;
            }
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
                    LocalSessionLifecycleEvent::PinChanged(_) => {
                        return Err(anyhow!("pin event entered operation recovery"));
                    }
                    LocalSessionLifecycleEvent::DisplayNameChanged(_) => {
                        return Err(anyhow!("display-name event entered operation recovery"));
                    }
                    LocalSessionLifecycleEvent::GeneratedTitleChanged(_) => {
                        return Err(anyhow!("generated-title event entered operation recovery"));
                    }
                    LocalSessionLifecycleEvent::ArtifactGcCompleted(_) => {
                        return Err(anyhow!("artifact GC event entered operation recovery"));
                    }
                    LocalSessionLifecycleEvent::RetentionBatchCompleted(_) => (
                        LocalSessionLifecycleOperationKind::Retention,
                        LocalSessionLifecycleRecoveryStatus::Completed,
                    ),
                    LocalSessionLifecycleEvent::RetentionBatchPlanned(_) => (
                        LocalSessionLifecycleOperationKind::Retention,
                        LocalSessionLifecycleRecoveryStatus::Uncertain,
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
        let pins = self.session_pin_projection()?;
        let display_names = self.session_display_name_projection()?;
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
            let mut entry = self.catalog_entry(candidate, state);
            entry.pinned = entry.session_id.as_deref().is_some_and(|session_id| {
                pins.get(&entry.session_ref)
                    .is_some_and(|(pinned_session_id, pinned)| {
                        pinned_session_id == session_id && *pinned
                    })
            });
            if let Some(session_id) = entry.session_id.as_deref()
                && let Some((named_session_id, display_name)) =
                    display_names.get(&entry.session_ref)
                && named_session_id == session_id
            {
                entry.title = Some(display_name.clone());
            }
            entries.push(entry);
        }
        Ok(LocalSessionCatalog {
            entries,
            truncated_entry_count,
        })
    }

    /// Resolves one catalog candidate against current bounded lifecycle and JSONL truth.
    ///
    /// SQLite projection state is deliberately absent from this operation. The returned path is
    /// a canonical direct child of this service's session directory and is suitable only for a
    /// second durable binding check by the application runtime.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the source disappeared, is not a ready V2 stream, changed durable
    /// identity, or the current lifecycle catalog cannot be verified.
    pub fn resolve_session_for_reopen(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
    ) -> std::result::Result<LocalSessionReopenBinding, LocalSessionReopenError> {
        let reference = session_ref.as_path();
        if reference.components().count() != 1
            || reference.extension().and_then(|value| value.to_str()) != Some("jsonl")
        {
            return Err(LocalSessionReopenError::NotFound);
        }
        let directory_metadata = match fs::symlink_metadata(&self.session_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LocalSessionReopenError::NotFound);
            }
            Err(source) => {
                return Err(LocalSessionReopenError::CatalogUnavailable {
                    source: source.into(),
                });
            }
        };
        if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
            return Err(LocalSessionReopenError::CatalogUnavailable {
                source: anyhow!("configured session directory must be a real directory"),
            });
        }
        let session_dir = fs::canonicalize(&self.session_dir).map_err(|source| {
            LocalSessionReopenError::CatalogUnavailable {
                source: source.into(),
            }
        })?;
        let path = session_ref.resolve(&session_dir);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(LocalSessionReopenError::NotFound);
            }
            Err(source) => {
                return Err(LocalSessionReopenError::CatalogUnavailable {
                    source: source.into(),
                });
            }
        };
        let symlink_or_non_file = metadata.file_type().is_symlink() || !metadata.is_file();
        let canonical_path = if symlink_or_non_file {
            path
        } else {
            fs::canonicalize(&path).map_err(|source| {
                LocalSessionReopenError::CatalogUnavailable {
                    source: source.into(),
                }
            })?
        };
        let bytes = metadata.len();
        let state = if symlink_or_non_file {
            LocalSessionCatalogState::Invalid
        } else if bytes > self.limits.max_stream_bytes {
            LocalSessionCatalogState::Oversized
        } else if bytes > self.limits.max_total_validation_bytes {
            LocalSessionCatalogState::ScanBudgetExceeded
        } else {
            LocalSessionCatalogState::Ready
        };
        let entry = self.catalog_entry(
            SessionCandidate {
                session_ref: session_ref.clone(),
                path: canonical_path,
                bytes,
                modified_at_unix_ms: modified_at_unix_ms(&metadata),
                symlink_or_non_file,
            },
            state,
        );
        if entry.state != LocalSessionCatalogState::Ready {
            return Err(LocalSessionReopenError::NotReady { state: entry.state });
        }
        let Some(session_id) = entry.session_id else {
            return Err(LocalSessionReopenError::NotReady {
                state: LocalSessionCatalogState::Invalid,
            });
        };
        if session_id != expected_session_id {
            return Err(LocalSessionReopenError::IdentityChanged);
        }
        Ok(LocalSessionReopenBinding {
            session_ref: entry.session_ref,
            session_id,
            session_log_path: entry.path,
        })
    }

    /// Forks one exact finalized conversation turn into a new direct session child.
    ///
    /// The source stream and workspace files remain unchanged. Only the safe conversation prefix
    /// and rebound external provenance are copied into the destination.
    ///
    /// # Errors
    ///
    /// Returns an error when the source identity or turn digest is stale, the source is not a
    /// ready V2 stream, or the destination cannot be created safely.
    pub fn fork_session_at_turn(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
        source_turn_digest: &str,
        destination_key: &str,
        root_config: &RootConfig,
        target_model_ref: &sigil_kernel::ModelRef,
    ) -> Result<ConversationForkOutput> {
        if source_turn_digest.trim().is_empty() || destination_key.trim().is_empty() {
            bail!("conversation fork turn digest and destination key must not be empty");
        }
        let binding = self
            .resolve_session_for_reopen(session_ref, expected_session_id)
            .map_err(|error| anyhow!(error))?;
        let records = JsonlSessionStore::read_event_records(&binding.session_log_path)
            .with_context(|| format!("failed to read {}", binding.session_log_path.display()))?;
        let projection = project_records(&records)?;
        let _ = projection.resolved_model_route.as_ref();
        let (provider_name, resolved_model_route) =
            crate::provider_connections::resolve_model_route(root_config, target_model_ref)
                .map_err(anyhow::Error::new)?;
        let model_name = resolved_model_route.model_ref.model_id.clone();
        let parent = binding
            .session_log_path
            .parent()
            .ok_or_else(|| anyhow!("source session has no parent directory"))?;
        let destination_path = conversation_fork_path(
            parent,
            expected_session_id,
            source_turn_digest,
            destination_key,
        );
        if destination_path.exists() {
            return recover_conversation_fork_output(
                &destination_path,
                expected_session_id,
                source_turn_digest,
            );
        }
        let store = JsonlSessionStore::new(&binding.session_log_path)?;
        fork_conversation_at_turn(
            &store,
            &records,
            &ConversationTurnForkRequest {
                source_turn_digest: source_turn_digest.to_owned(),
                source_session_ref: binding.session_ref,
                destination_path,
                provider_name,
                model_name,
                resolved_model_route: Some(resolved_model_route),
            },
        )
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
        self.export_session_with_artifacts(
            source_path,
            destination,
            exported_at_unix_ms,
            SessionArtifactExportModeV1::BoundedTranscript,
        )
    }

    /// Writes a content-bound export with an explicit raw tool-artifact policy.
    ///
    /// `BoundedTranscript` never copies raw artifact bytes and marks the export incomplete when
    /// the source contained published or unavailable tool output. `IncludeArtifacts` embeds each
    /// immutable body as base64. `RejectIfIncomplete` refuses sources with any artifact dependency.
    ///
    /// # Errors
    ///
    /// Returns an error for source drift, an unsafe destination, missing/corrupt artifacts, an
    /// export budget violation, or a failed lifecycle journal append.
    pub fn export_session_with_artifacts(
        &self,
        source_path: &Path,
        destination: Option<&Path>,
        exported_at_unix_ms: u64,
        artifact_mode: SessionArtifactExportModeV1,
    ) -> Result<SessionExportOutput> {
        let source = self.resolve_ready_source(source_path)?;
        let before_hash = hash_file_bounded(&source.path, self.limits.max_stream_bytes)?;
        let records = JsonlSessionStore::read_event_records(&source.path)?;
        let projection = project_records(&records)?;
        let artifact_store = ToolArtifactStore::for_session_path(&source.path);
        let tool_artifacts = export_tool_artifacts(
            &artifact_store,
            &projection.tool_artifacts,
            projection.unavailable_tool_result_count,
            artifact_mode,
            self.limits.max_export_bytes,
        )?;
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
            tool_artifacts,
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
            artifact_mode,
            artifact_completeness: artifact.payload.tool_artifacts.completeness,
            included_artifact_count: artifact.payload.tool_artifacts.included_artifact_count,
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
            artifact_completeness: artifact.payload.tool_artifacts.completeness,
            included_artifact_count: artifact.payload.tool_artifacts.included_artifact_count,
            journal_sequence: completed.sequence,
        })
    }

    /// Runs manifest-only artifact GC for one exact session identity.
    ///
    /// `roots` must come from the active incremental descriptor/context/read projections. The
    /// lifecycle service adds a whole-session hold when the session is pinned, then delegates to
    /// the store without reading or locking the session JSONL.
    ///
    /// # Errors
    ///
    /// Returns an error for a stale session identity, invalid roots, unsafe manifests, failed
    /// tombstone moves, or a failed bounded audit append.
    pub fn garbage_collect_session_artifacts(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
        mut roots: ToolArtifactGcRootsV1,
        now_unix_ms: u64,
    ) -> Result<ToolArtifactGcReportV1> {
        let _maintenance = self.acquire_maintenance_lease()?;
        let store = self.resolve_artifact_store_without_jsonl(session_ref, expected_session_id)?;
        if self.is_session_pinned(session_ref, expected_session_id)? {
            roots.explicit_holds.extend(
                store
                    .manifest_inventory()?
                    .into_iter()
                    .map(|entry| entry.descriptor.artifact_ref),
            );
        }
        let report = store.garbage_collect(
            &roots,
            now_unix_ms,
            sigil_kernel::session::TOOL_ARTIFACT_ORPHAN_GRACE_MS,
        )?;
        let operation_id = format!("session-artifact-gc:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            now_unix_ms,
            LocalSessionLifecycleEvent::ArtifactGcCompleted(LocalSessionArtifactGcJournalBinding {
                source_session_ref: session_ref.clone(),
                source_session_id: expected_session_id.to_owned(),
                tombstone_id: report.tombstone_id.clone(),
                scanned_manifests: report.scanned_manifests,
                tombstoned_manifests: report.tombstoned_manifests,
                tombstoned_blobs: report.tombstoned_blobs,
                tombstoned_orphan_blobs: report.tombstoned_orphan_blobs,
                tombstoned_staging_files: report.tombstoned_staging_files,
                tombstoned_bytes: report.tombstoned_bytes,
                skipped_active_reads: report.skipped_active_reads,
            }),
        )?;
        Ok(report)
    }

    /// Appends a durable pin/unpin decision for one exact session identity.
    ///
    /// # Errors
    ///
    /// Returns an error when the source is not a ready direct V2 session or the lifecycle journal
    /// cannot durably append the decision.
    pub fn set_session_pin(
        &self,
        source_path: &Path,
        pinned: bool,
        recorded_at_unix_ms: u64,
    ) -> Result<LocalSessionLifecycleRecord> {
        let _maintenance = self.acquire_maintenance_lease()?;
        let source = self.resolve_ready_source(source_path)?;
        let source_session_id = source
            .session_id
            .clone()
            .ok_or_else(|| anyhow!("source session has no durable identity"))?;
        let operation_id = format!("session-pin:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            recorded_at_unix_ms,
            LocalSessionLifecycleEvent::PinChanged(LocalSessionPinJournalBinding {
                source_session_ref: source.session_ref,
                source_session_id,
                pinned,
            }),
        )
    }

    /// Appends a bounded display-name override for one exact durable session identity.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the name is empty/unsafe, the source is missing or changed, or
    /// the lifecycle journal cannot durably append the decision.
    pub fn rename_session(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
        display_name: &str,
        recorded_at_unix_ms: u64,
    ) -> std::result::Result<LocalSessionLifecycleRecord, LocalSessionMutationError> {
        if display_name.is_empty()
            || display_name.trim() != display_name
            || display_name.len() > SESSION_TITLE_MAX_BYTES
            || safe_persistence_text(display_name) != display_name
        {
            return Err(LocalSessionMutationError::InvalidRequest);
        }
        let _maintenance = self
            .acquire_maintenance_lease()
            .map_err(|source| LocalSessionMutationError::Unavailable { source })?;
        self.resolve_session_for_reopen(session_ref, expected_session_id)
            .map_err(map_session_reopen_mutation_error)?;
        let operation_id = format!("session-display-name:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal()
            .append(
                &operation_id,
                recorded_at_unix_ms,
                LocalSessionLifecycleEvent::DisplayNameChanged(
                    LocalSessionDisplayNameJournalBinding {
                        source_session_ref: session_ref.clone(),
                        source_session_id: expected_session_id.to_owned(),
                        display_name: display_name.to_owned(),
                    },
                ),
            )
            .map_err(|source| LocalSessionMutationError::Unavailable { source })
    }

    /// Appends one bounded system-generated title without opening or locking the active session.
    ///
    /// The caller must already own the exact durable session identity. Manual display-name events
    /// always win in projection, even when a generated title finishes after a user rename.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_generated_title(
        &self,
        session_ref: &SessionRef,
        session_id: &str,
        title: &str,
        provider_name: &str,
        model_name: &str,
        prompt_tokens: Option<u64>,
        completion_tokens: Option<u64>,
        recorded_at_unix_ms: u64,
    ) -> Result<LocalSessionLifecycleRecord> {
        if session_id.trim().is_empty()
            || session_id.len() > 256
            || title.is_empty()
            || title.trim() != title
            || title.len() > SESSION_TITLE_MAX_BYTES
            || safe_persistence_text(title) != title
            || provider_name.trim().is_empty()
            || provider_name.len() > 128
            || model_name.trim().is_empty()
            || model_name.len() > 256
        {
            bail!("generated session title binding is invalid");
        }
        let operation_id = format!("session-generated-title:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            recorded_at_unix_ms,
            LocalSessionLifecycleEvent::GeneratedTitleChanged(
                LocalSessionGeneratedTitleJournalBinding {
                    source_session_ref: session_ref.clone(),
                    source_session_id: session_id.to_owned(),
                    title: title.to_owned(),
                    provider_name: provider_name.to_owned(),
                    model_name: model_name.to_owned(),
                    prompt_tokens,
                    completion_tokens,
                },
            ),
        )
    }

    /// Deletes one exact, unpinned durable session after rebuilding and revalidating its preview.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the source is missing, changed, pinned, protected, or currently
    /// unavailable for an audited delete.
    pub fn delete_session(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
        protected_paths: &[PathBuf],
        applied_at_unix_ms: u64,
    ) -> std::result::Result<SessionDeleteOutput, LocalSessionMutationError> {
        let binding = self
            .resolve_session_for_reopen(session_ref, expected_session_id)
            .map_err(map_session_reopen_mutation_error)?;
        if self
            .is_session_pinned(session_ref, expected_session_id)
            .map_err(|source| LocalSessionMutationError::Unavailable { source })?
        {
            return Err(LocalSessionMutationError::Pinned);
        }
        let preview = self
            .preview_delete(&binding.session_log_path, protected_paths)
            .map_err(|source| LocalSessionMutationError::Unavailable { source })?;
        self.apply_delete(&preview, protected_paths, applied_at_unix_ms)
            .map_err(|source| LocalSessionMutationError::Unavailable { source })
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
        self.preview_delete_entry(&source, protected_paths)
    }

    fn preview_delete_entry(
        &self,
        source: &LocalSessionCatalogEntry,
        protected_paths: &[PathBuf],
    ) -> Result<SessionDeletePreview> {
        if source.pinned {
            bail!("pinned session cannot be deleted");
        }
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
            .clone()
            .ok_or_else(|| anyhow!("source session has no durable identity"))?;
        let resource_path = session_resource_path(&source.path)?;
        let (resource_tree_sha256, resource_bytes) = hash_directory_tree_bounded(&resource_path)?;
        let preview_digest = delete_preview_digest(
            &self.workspace_id,
            &source.session_ref,
            &source_session_id,
            &source_content_sha256,
            source_bytes,
            source_modified_at_unix_ms,
            &resource_tree_sha256,
            resource_bytes,
        )?;
        Ok(SessionDeletePreview {
            source_path: source.path.clone(),
            source_session_ref: source.session_ref.clone(),
            source_session_id,
            source_content_sha256,
            source_bytes,
            source_modified_at_unix_ms,
            resource_tree_sha256,
            resource_bytes,
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
        let _maintenance = self.acquire_maintenance_lease()?;
        if self.is_session_pinned(&preview.source_session_ref, &preview.source_session_id)? {
            bail!("pinned session cannot be deleted");
        }
        let lease = self.preflight_delete(preview, protected_paths)?;
        self.apply_delete_after_preflight(preview, lease, applied_at_unix_ms)
    }

    fn preflight_delete(
        &self,
        preview: &SessionDeletePreview,
        protected_paths: &[PathBuf],
    ) -> Result<File> {
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
        let resource_path = session_resource_path(&preview.source_path)?;
        let (resource_tree_sha256, resource_bytes) = hash_directory_tree_bounded(&resource_path)?;
        if resource_tree_sha256 != preview.resource_tree_sha256
            || resource_bytes != preview.resource_bytes
        {
            bail!("session resources changed after delete preview");
        }
        Ok(lease)
    }

    fn apply_delete_after_preflight(
        &self,
        preview: &SessionDeletePreview,
        lease: File,
        applied_at_unix_ms: u64,
    ) -> Result<SessionDeleteOutput> {
        let tombstone_id = format!("session-delete-{}", uuid::Uuid::new_v4().simple());
        let binding = LocalSessionDeleteJournalBinding {
            source_session_ref: preview.source_session_ref.clone(),
            source_session_id: preview.source_session_id.clone(),
            source_content_sha256: preview.source_content_sha256.clone(),
            source_bytes: preview.source_bytes,
            source_modified_at_unix_ms: preview.source_modified_at_unix_ms,
            resource_tree_sha256: preview.resource_tree_sha256.clone(),
            resource_bytes: preview.resource_bytes,
            tombstone_id: tombstone_id.clone(),
            preview_digest: preview.preview_digest.clone(),
        };
        let operation_id = format!("session-delete:{}", uuid::Uuid::new_v4());
        self.lifecycle_journal().append(
            &operation_id,
            applied_at_unix_ms,
            LocalSessionLifecycleEvent::DeletePlanned(binding.clone()),
        )?;
        move_session_to_tombstone(&preview.source_path, &tombstone_id)?;
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
            tombstoned_resource_bytes: preview.resource_bytes,
            tombstone_id,
            journal_sequence: completed.sequence,
        })
    }

    /// Permanently unlinks session tombstones only after the mandatory grace period.
    ///
    /// This operation never scans active JSONL streams and rejects symlinked trash entries.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe tombstone entries, a read-lease inspection failure, or failed
    /// unlink.
    pub fn prune_delete_tombstones(&self, now_unix_ms: u64) -> Result<SessionTombstonePruneOutput> {
        let _maintenance = self.acquire_maintenance_lease()?;
        let trash = self.session_dir.join(".session-trash");
        let entries = match fs::read_dir(&trash) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionTombstonePruneOutput {
                    removed_tombstones: 0,
                    removed_bytes: 0,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", trash.display()));
            }
        };
        let mut removed_tombstones = 0usize;
        let mut removed_bytes = 0u64;
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read {}", trash.display()))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("session tombstone trash contains an unsafe entry");
            }
            if now_unix_ms.saturating_sub(modified_at_unix_ms(&metadata))
                < SESSION_DELETE_TOMBSTONE_GRACE_MS
            {
                continue;
            }
            let Some(_artifact_read_leases) = acquire_tombstone_artifact_locks(&path)? else {
                continue;
            };
            let bytes = measure_directory_tree_bounded(&path)?;
            fs::remove_dir_all(&path)
                .with_context(|| format!("failed to prune {}", path.display()))?;
            removed_tombstones = removed_tombstones.saturating_add(1);
            removed_bytes = removed_bytes.saturating_add(bytes);
        }
        sync_directory(&trash)?;
        Ok(SessionTombstonePruneOutput {
            removed_tombstones,
            removed_bytes,
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
            pinned: false,
        };
        if initial_state != LocalSessionCatalogState::Ready {
            return entry;
        }
        let records = match JsonlSessionStore::read_event_records(&candidate.path) {
            Ok(records) => records,
            Err(_) => {
                entry.state = LocalSessionCatalogState::Invalid;
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

    fn resolve_artifact_store_without_jsonl(
        &self,
        session_ref: &SessionRef,
        expected_session_id: &str,
    ) -> Result<ToolArtifactStore> {
        let relative = session_ref.as_path();
        if relative.components().count() != 1
            || relative.extension().and_then(|value| value.to_str()) != Some("jsonl")
        {
            bail!("artifact GC source must be a direct session reference");
        }
        let directory_metadata = fs::symlink_metadata(&self.session_dir)
            .with_context(|| format!("failed to inspect {}", self.session_dir.display()))?;
        if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
            bail!("configured session directory must be a real directory");
        }
        let directory = fs::canonicalize(&self.session_dir)
            .with_context(|| format!("failed to canonicalize {}", self.session_dir.display()))?;
        let path = session_ref.resolve(&directory);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("artifact GC source must be a real session file");
        }
        let path = fs::canonicalize(&path)
            .with_context(|| format!("failed to canonicalize {}", path.display()))?;
        if path.parent() != Some(directory.as_path()) {
            bail!("artifact GC source escaped the configured session directory");
        }
        let store = ToolArtifactStore::for_session_path(&path);
        if store.session_scope_id() != expected_session_id {
            bail!("artifact GC session identity changed");
        }
        Ok(store)
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

    fn session_pin_projection(&self) -> Result<BTreeMap<SessionRef, (String, bool)>> {
        let mut pins = BTreeMap::new();
        for record in self.lifecycle_records()? {
            if let LocalSessionLifecycleEvent::PinChanged(binding) = record.event {
                pins.insert(
                    binding.source_session_ref,
                    (binding.source_session_id, binding.pinned),
                );
            }
        }
        Ok(pins)
    }

    fn session_display_name_projection(&self) -> Result<BTreeMap<SessionRef, (String, String)>> {
        let mut generated_titles = BTreeMap::new();
        let mut manual_names = BTreeMap::new();
        for record in self.lifecycle_records()? {
            match record.event {
                LocalSessionLifecycleEvent::DisplayNameChanged(binding) => {
                    manual_names.insert(
                        binding.source_session_ref,
                        (binding.source_session_id, binding.display_name),
                    );
                }
                LocalSessionLifecycleEvent::GeneratedTitleChanged(binding) => {
                    generated_titles.insert(
                        binding.source_session_ref,
                        (binding.source_session_id, binding.title),
                    );
                }
                _ => {}
            }
        }
        for (session_ref, generated_title) in generated_titles {
            manual_names.entry(session_ref).or_insert(generated_title);
        }
        Ok(manual_names)
    }

    fn is_session_pinned(&self, session_ref: &SessionRef, session_id: &str) -> Result<bool> {
        Ok(self
            .session_pin_projection()?
            .get(session_ref)
            .is_some_and(|(pinned_session_id, pinned)| pinned_session_id == session_id && *pinned))
    }

    fn acquire_maintenance_lease(&self) -> Result<File> {
        let name = self
            .lifecycle_journal_path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("session-lifecycle-v1.jsonl");
        let path = self
            .lifecycle_journal_path
            .with_file_name(format!("{name}.maintenance-lock"));
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("session maintenance lease has no parent"))?;
        if parent.exists() {
            let metadata = fs::symlink_metadata(parent)
                .with_context(|| format!("failed to inspect {}", parent.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("session maintenance lease parent must be a real directory");
            }
        } else {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        if path.exists()
            && fs::symlink_metadata(&path)
                .with_context(|| format!("failed to inspect {}", path.display()))?
                .file_type()
                .is_symlink()
        {
            bail!("session maintenance lease must not be a symlink");
        }
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let lease = options
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        harden_private_open_file(&lease, &path, "session maintenance lease")?;
        lease
            .try_lock()
            .context("another local session maintenance operation is active")?;
        Ok(lease)
    }

    fn recover_incomplete_delete(
        &self,
        binding: &LocalSessionDeleteJournalBinding,
    ) -> LocalSessionLifecycleRecoveryStatus {
        let path = binding.source_session_ref.resolve(&self.session_dir);
        if !path.exists() {
            let tombstoned = self
                .session_dir
                .join(".session-trash")
                .join(&binding.tombstone_id)
                .join("session.jsonl");
            return match hash_file_bounded(&tombstoned, self.limits.max_stream_bytes) {
                Ok(hash) if hash == binding.source_content_sha256 => {
                    LocalSessionLifecycleRecoveryStatus::Completed
                }
                Ok(_) | Err(_) => LocalSessionLifecycleRecoveryStatus::Uncertain,
            };
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

fn conversation_fork_path(
    parent: &Path,
    source_session_id: &str,
    source_turn_digest: &str,
    destination_key: &str,
) -> PathBuf {
    let identity = sigil_kernel::stable_event_uuid(
        "sigil-runtime-conversation-fork-path",
        &format!("{source_session_id}:{source_turn_digest}:{destination_key}"),
    );
    parent.join(format!("session-fork-{identity}.jsonl"))
}

fn recover_conversation_fork_output(
    destination_path: &Path,
    expected_source_session_id: &str,
    expected_source_turn_digest: &str,
) -> Result<ConversationForkOutput> {
    let records = JsonlSessionStore::read_event_records(destination_path)?;
    let (fork_event, forked) = records
        .iter()
        .find_map(|record| {
            let SessionStreamRecord::Stored(event) = record;
            (event.event_kind() == Some(DurableEventType::ConversationForked)).then(|| {
                serde_json::from_value::<ConversationForked>(event.payload.clone())
                    .map(|forked| (event.clone(), forked))
            })
        })
        .transpose()?
        .context("existing conversation fork has no durable fork receipt")?;
    if forked.source_session_id != expected_source_session_id
        || forked.source_turn_digest != expected_source_turn_digest
    {
        bail!("existing conversation fork destination belongs to another source binding");
    }
    Ok(ConversationForkOutput {
        destination_session_ref: SessionRef::new_relative(
            destination_path
                .file_name()
                .context("conversation fork destination has no file name")?,
        )?,
        destination_path: destination_path.to_path_buf(),
        destination_session_id: forked.destination_session_id,
        fork_event,
        copied_message_count: forked.copied_message_count,
        copied_external_provenance_count: forked.copied_external_provenance_count,
    })
}

fn map_session_reopen_mutation_error(error: LocalSessionReopenError) -> LocalSessionMutationError {
    match error {
        LocalSessionReopenError::NotFound => LocalSessionMutationError::NotFound,
        LocalSessionReopenError::NotReady { .. } => LocalSessionMutationError::NotReady,
        LocalSessionReopenError::IdentityChanged => LocalSessionMutationError::IdentityChanged,
        LocalSessionReopenError::CatalogUnavailable { source } => {
            LocalSessionMutationError::Unavailable { source }
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
    resolved_model_route: Option<sigil_kernel::ResolvedModelRoute>,
    title: Option<String>,
    messages: Vec<ModelMessage>,
    external_provenance: Vec<ExternalProvenanceEntry>,
    tool_artifacts: Vec<ToolArtifactDescriptorV1>,
    unavailable_tool_result_count: usize,
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
            SessionLogEntry::User(message) | SessionLogEntry::Assistant(message) => {
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
            SessionLogEntry::ToolResultV2(result) => {
                match &result.artifact {
                    ToolArtifactBindingV1::Published { descriptor } => {
                        projection.tool_artifacts.push(descriptor.clone());
                    }
                    ToolArtifactBindingV1::Unavailable { .. } => {
                        projection.unavailable_tool_result_count =
                            projection.unavailable_tool_result_count.saturating_add(1);
                    }
                }
                let message = result.model_message()?;
                messages_by_id.insert(message.id.clone(), message.clone());
                projection.messages.push(message);
            }
            SessionLogEntry::Control(ControlEntry::SessionIdentity {
                provider_name,
                model_name,
                resolved_model_route,
            }) => {
                projection.provider_name.get_or_insert(provider_name);
                projection.model_name.get_or_insert(model_name);
                if projection.resolved_model_route.is_none() {
                    projection.resolved_model_route = resolved_model_route;
                }
            }
            SessionLogEntry::Control(ControlEntry::SessionModelSelected { model_name }) => {
                projection.model_name = Some(model_name);
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
    sigil_kernel::conversation_transcript_entry_from_record(record)
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
            image_attachments: message
                .image_attachments
                .iter()
                .map(sigil_kernel::ImageAttachment::without_resolved_bytes)
                .collect(),
        })
        .collect())
}

fn export_tool_artifacts(
    store: &ToolArtifactStore,
    descriptors: &[ToolArtifactDescriptorV1],
    unavailable_tool_result_count: usize,
    mode: SessionArtifactExportModeV1,
    max_export_bytes: usize,
) -> Result<SessionExportToolArtifactsV1> {
    if mode == SessionArtifactExportModeV1::RejectIfIncomplete
        && (!descriptors.is_empty() || unavailable_tool_result_count > 0)
    {
        bail!("session export would be incomplete without tool artifacts");
    }
    let mut artifacts = Vec::new();
    if mode == SessionArtifactExportModeV1::IncludeArtifacts {
        let estimated_artifact_bytes =
            descriptors.iter().try_fold(0usize, |total, descriptor| {
                let encoded_body = usize::try_from(descriptor.persisted_bytes)
                    .unwrap_or(usize::MAX)
                    .div_ceil(3)
                    .saturating_mul(4);
                let descriptor_bytes = serde_json::to_vec(descriptor)
                    .context("failed to encode tool artifact export descriptor")?
                    .len();
                Ok::<_, anyhow::Error>(
                    total
                        .saturating_add(encoded_body)
                        .saturating_add(descriptor_bytes),
                )
            })?;
        if estimated_artifact_bytes > max_export_bytes {
            bail!("tool artifacts exceed the configured export byte limit");
        }
        artifacts.reserve(descriptors.len());
        for descriptor in descriptors {
            let body = store.read_all(descriptor)?;
            artifacts.push(SessionExportToolArtifactV1 {
                descriptor: descriptor.clone(),
                body_base64: BASE64_STANDARD.encode(body),
            });
        }
    }
    let included_artifact_count = artifacts.len();
    let omitted_artifact_count = descriptors.len().saturating_sub(included_artifact_count);
    let completeness = if omitted_artifact_count == 0 && unavailable_tool_result_count == 0 {
        SessionArtifactExportCompletenessV1::Complete
    } else {
        SessionArtifactExportCompletenessV1::Incomplete
    };
    Ok(SessionExportToolArtifactsV1 {
        mode,
        completeness,
        published_artifact_count: descriptors.len(),
        included_artifact_count,
        omitted_artifact_count,
        unavailable_tool_result_count,
        artifacts,
    })
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
                    image_attachments: message.image_attachments.clone(),
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
    resource_tree_sha256: &str,
    resource_bytes: u64,
) -> Result<String> {
    digest_serializable(&(
        workspace_id,
        source_session_ref,
        source_session_id,
        source_content_sha256,
        source_bytes,
        source_modified_at_unix_ms,
        resource_tree_sha256,
        resource_bytes,
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
        &preview.resource_tree_sha256,
        preview.resource_bytes,
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
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    let lease = options
        .open(&lease_path)
        .with_context(|| format!("failed to open {}", lease_path.display()))?;
    harden_private_open_file(&lease, &lease_path, "session writer lease")?;
    lease.try_lock().with_context(|| {
        format!(
            "session is active or its writer lease is busy: {}",
            source_path.display()
        )
    })?;
    Ok(lease)
}

fn session_writer_is_inactive(source_path: &Path) -> Result<bool> {
    let file_name = source_path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("source session file name is invalid"))?;
    let lease_path = source_path.with_file_name(format!("{file_name}.writer-lock"));
    if !lease_path.exists() {
        return Ok(true);
    }
    let metadata = fs::symlink_metadata(&lease_path)
        .with_context(|| format!("failed to inspect {}", lease_path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("session writer lease must be a regular file");
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW);
    let lease = options
        .open(&lease_path)
        .with_context(|| format!("failed to open {}", lease_path.display()))?;
    if !lease
        .metadata()
        .with_context(|| format!("failed to inspect {}", lease_path.display()))?
        .is_file()
    {
        bail!("session writer lease must be a regular file");
    }
    match lease.try_lock() {
        Ok(()) => Ok(true),
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(error).with_context(|| {
            format!(
                "failed to inspect session writer activity: {}",
                source_path.display()
            )
        }),
    }
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

fn session_resource_path(session_path: &Path) -> Result<PathBuf> {
    let parent = session_path
        .parent()
        .ok_or_else(|| anyhow!("source session has no parent directory"))?;
    let stem = session_path
        .file_stem()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("source session has no resource stem"))?;
    Ok(parent.join(stem))
}

fn hash_directory_tree_bounded(path: &Path) -> Result<(String, u64)> {
    if !path.exists() {
        return Ok((format!("{:x}", Sha256::digest(b"empty-directory-tree")), 0));
    }
    let files = collect_directory_tree_files_bounded(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    for file_path in files {
        let relative = file_path
            .strip_prefix(path)
            .context("session resource escaped its root")?;
        let relative = relative
            .to_str()
            .ok_or_else(|| anyhow!("session resource path is not UTF-8"))?;
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        let mut file = File::open(&file_path)
            .with_context(|| format!("failed to open {}", file_path.display()))?;
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("failed to read {}", file_path.display()))?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > SESSION_RESOURCE_MAX_BYTES {
                bail!("session resource tree exceeds its byte limit");
            }
            hasher.update(&buffer[..read]);
        }
    }
    Ok((format!("{:x}", hasher.finalize()), total))
}

fn measure_directory_tree_bounded(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0u64;
    for file_path in collect_directory_tree_files_bounded(path)? {
        let metadata = fs::symlink_metadata(&file_path)
            .with_context(|| format!("failed to inspect {}", file_path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("session resource tree contains an unsafe file");
        }
        total = total.saturating_add(metadata.len());
        if total > SESSION_RESOURCE_MAX_BYTES {
            bail!("session resource tree exceeds its byte limit");
        }
    }
    Ok(total)
}

fn collect_directory_tree_files_bounded(path: &Path) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("session resource root must be a real directory");
    }
    let mut pending = vec![path.to_path_buf()];
    let mut files = Vec::new();
    let mut entry_count = 0usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("failed to read {}", directory.display()))?
        {
            let entry = entry.with_context(|| format!("failed to read {}", directory.display()))?;
            entry_count = entry_count.saturating_add(1);
            if entry_count > SESSION_RESOURCE_MAX_ENTRIES {
                bail!("session resource tree exceeds its entry limit");
            }
            let entry_path = entry.path();
            let metadata = fs::symlink_metadata(&entry_path)
                .with_context(|| format!("failed to inspect {}", entry_path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!("session resource tree must not contain symlinks");
            }
            if metadata.is_dir() {
                pending.push(entry_path);
            } else if metadata.is_file() {
                files.push(entry_path);
            } else {
                bail!("session resource tree contains a non-file entry");
            }
        }
    }
    files.sort();
    Ok(files)
}

fn move_session_to_tombstone(source_path: &Path, tombstone_id: &str) -> Result<()> {
    if tombstone_id.is_empty()
        || tombstone_id.len() > 128
        || !tombstone_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        bail!("session tombstone identity is malformed");
    }
    let session_parent = source_path
        .parent()
        .ok_or_else(|| anyhow!("source session has no parent directory"))?;
    let trash = session_parent.join(".session-trash");
    if trash.exists() {
        let metadata = fs::symlink_metadata(&trash)
            .with_context(|| format!("failed to inspect {}", trash.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("session tombstone root must be a real directory");
        }
    } else {
        fs::create_dir(&trash).with_context(|| format!("failed to create {}", trash.display()))?;
    }
    let tombstone = trash.join(tombstone_id);
    fs::create_dir(&tombstone)
        .with_context(|| format!("failed to create {}", tombstone.display()))?;
    let tombstoned_session = tombstone.join("session.jsonl");
    fs::rename(source_path, &tombstoned_session)
        .with_context(|| format!("failed to tombstone {}", source_path.display()))?;
    let resource_path = session_resource_path(source_path)?;
    if resource_path.exists() {
        let destination = tombstone.join("resources");
        if let Err(error) = fs::rename(&resource_path, &destination) {
            let rollback = fs::rename(&tombstoned_session, source_path);
            if rollback.is_ok() {
                let _ = fs::remove_dir(&tombstone);
            }
            return Err(error)
                .with_context(|| format!("failed to tombstone {}", resource_path.display()));
        }
    }
    sync_directory(&tombstone)?;
    sync_directory(&trash)?;
    sync_directory(session_parent)
}

fn acquire_tombstone_artifact_locks(tombstone: &Path) -> Result<Option<Vec<File>>> {
    let locks_dir = tombstone.join("resources").join("artifacts").join("locks");
    let entries = match fs::read_dir(&locks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", locks_dir.display()));
        }
    };
    let mut leases = Vec::new();
    for entry in entries {
        let entry = entry.with_context(|| format!("failed to read {}", locks_dir.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("session tombstone contains an unsafe artifact lock");
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW);
        let lease = options
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        match lease.try_lock() {
            Ok(()) => leases.push(lease),
            Err(fs::TryLockError::WouldBlock) => return Ok(None),
            Err(fs::TryLockError::Error(error)) => {
                return Err(error).with_context(|| {
                    format!("failed to lock tombstoned artifact {}", path.display())
                });
            }
        }
    }
    Ok(Some(leases))
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
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        harden_private_open_file(&file, &temporary, "session export temporary file")?;
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

fn harden_private_open_file(file: &File, path: &Path, label: &str) -> Result<()> {
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if !metadata.is_file() {
        bail!("{label} must be a regular file");
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o7777 != 0o600 {
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to restrict {}", path.display()))?;
    }
    #[cfg(windows)]
    if !sigil_kernel::private_path_permissions_are_restricted(path)? {
        sigil_kernel::secure_private_path_permissions(path)?;
    }
    Ok(())
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
