use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use async_trait::async_trait;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, ContextBodyRef, ContextInclusionReason, ContextItem, ContextScoreComponent,
    ContextScoreComponentKind, ContextSensitivity, ContextSource, ContextTrustLevel,
    MEMORY_STATEMENT_MAX_BYTES, Tool, ToolAccess, ToolAnalysisStatus, ToolCategory, ToolContext,
    ToolMutationTracking, ToolOperation, ToolPermissionEffect, ToolPermissionPlanDraft,
    ToolPermissionSummary, ToolPreview, ToolPreviewCapability, ToolRegistry, ToolResult,
    ToolResultMeta, ToolSemanticScope, ToolSpec, ToolSubject, ToolSubjectKind, ToolSubjectScope,
    atomic_publish_private_file, estimate_context_token_cost, remember_memory_tool_spec,
    safe_persistence_text, secure_private_path_permissions,
};

pub use sigil_kernel::{REMEMBER_PROJECT_FACT_TOOL_NAME, REMEMBER_USER_PREFERENCE_TOOL_NAME};

use crate::paths::SigilPaths;

pub const INSPECT_MEMORY_TOOL_NAME: &str = "inspect_memory";
pub const FORGET_MEMORY_TOOL_NAME: &str = "forget_memory";
const MEMORY_SCHEMA_VERSION: u32 = 1;
const MEMORY_STATEMENT_MAX_LINES: usize = 12;
const MEMORY_SIDECAR_MAX_BYTES: u64 = 8 * 1024;
const MEMORY_JOURNAL_MAX_BYTES: u64 = 16 * 1024 * 1024;
const MEMORY_JOURNAL_MAX_RECORDS: usize = 50_000;
const MEMORY_SCOPE_MAX_ENTRIES: usize = 512;
const MEMORY_CONTEXT_MAX_PREFERENCES: usize = 24;
const MEMORY_CONTEXT_MAX_PROJECT_FACTS: usize = 12;
const MEMORY_CONTEXT_MAX_TOKENS: usize = 1_536;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WritableMemoryScope {
    UserPreference,
    ProjectFact,
}

impl WritableMemoryScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::UserPreference => "user_preference",
            Self::ProjectFact => "project_fact",
        }
    }

    fn context_source(self) -> ContextSource {
        match self {
            Self::UserPreference => ContextSource::UserMemory,
            Self::ProjectFact => ContextSource::ProjectMemory,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MemorySourceRefV1 {
    session_scope_id: String,
    logical_run_id: String,
    tool_call_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MemorySidecarV1 {
    schema_version: u32,
    memory_id: String,
    logical_memory_id: String,
    version: u32,
    scope: WritableMemoryScope,
    workspace_id: Option<String>,
    statement: String,
    content_sha256: String,
    trust: String,
    validity: String,
    sensitivity: String,
    created_at_ms: u64,
    source: MemorySourceRefV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MemoryJournalAction {
    Admitted,
    Tombstoned,
    Deleted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct MemoryJournalRecordV1 {
    schema_version: u32,
    event_id: String,
    action: MemoryJournalAction,
    scope: WritableMemoryScope,
    workspace_id: Option<String>,
    memory_ids: Vec<String>,
    occurred_at_ms: u64,
    source_session_scope_id: Option<String>,
    source_logical_run_id: Option<String>,
    source_tool_call_id: Option<String>,
}

impl MemoryJournalRecordV1 {
    fn validate(&self, scope: WritableMemoryScope, workspace_id: Option<&str>) -> Result<()> {
        ensure!(
            self.schema_version == MEMORY_SCHEMA_VERSION,
            "unsupported writable-memory journal schema version"
        );
        validate_opaque_id(&self.event_id, "memory-event-")?;
        ensure!(
            self.scope == scope,
            "writable-memory journal scope mismatch"
        );
        ensure!(
            self.workspace_id.as_deref() == workspace_id,
            "writable-memory journal workspace mismatch"
        );
        ensure!(
            !self.memory_ids.is_empty() && self.memory_ids.len() <= MEMORY_CONTEXT_MAX_PREFERENCES,
            "writable-memory journal contains an invalid memory id set"
        );
        let mut unique = BTreeSet::new();
        for memory_id in &self.memory_ids {
            validate_opaque_id(memory_id, "memory-")?;
            ensure!(
                unique.insert(memory_id),
                "writable-memory journal duplicates a memory id"
            );
        }
        ensure!(
            self.memory_ids.len() == 1,
            "writable-memory lifecycle transition must target one memory"
        );
        ensure!(
            self.occurred_at_ms > 0,
            "writable-memory event time is invalid"
        );
        validate_optional_ref(self.source_session_scope_id.as_deref(), "session scope")?;
        validate_optional_ref(self.source_logical_run_id.as_deref(), "logical run")?;
        validate_optional_ref(self.source_tool_call_id.as_deref(), "tool call")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DurableMemoryReceipt {
    pub receipt_type: &'static str,
    pub durable: bool,
    pub created: bool,
    pub memory_id: String,
    pub logical_memory_id: String,
    pub version: u32,
    pub scope: WritableMemoryScope,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub stored_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DurableMemoryForgetReceipt {
    pub receipt_type: &'static str,
    pub durable: bool,
    pub memory_id: String,
    pub scope: WritableMemoryScope,
    pub tombstoned: bool,
    pub physical_deleted: bool,
    pub prior_provider_context_retracted: bool,
    pub completed_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DurableMemoryInspectEntry {
    pub memory_id: String,
    pub logical_memory_id: String,
    pub version: u32,
    pub scope: WritableMemoryScope,
    pub statement: String,
    pub trust: String,
    pub validity: String,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone)]
struct ProjectedMemory {
    sidecar: MemorySidecarV1,
    admission_event_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProjectedMemoryStatus {
    Active,
    Tombstoned,
    Deleted,
}

#[derive(Debug, Clone)]
struct ProjectedMemoryRef {
    status: ProjectedMemoryStatus,
    admission_event_id: String,
}

#[derive(Debug, Clone)]
struct MemoryScopeStore {
    root: PathBuf,
    scope: WritableMemoryScope,
    workspace_id: Option<String>,
}

impl MemoryScopeStore {
    fn new(root: PathBuf, scope: WritableMemoryScope, workspace_id: Option<String>) -> Self {
        Self {
            root,
            scope,
            workspace_id,
        }
    }

    fn entries_dir(&self) -> PathBuf {
        self.root.join("entries")
    }

    fn journal_path(&self) -> PathBuf {
        self.root.join("journal-v1.jsonl")
    }

    fn lease_path(&self) -> PathBuf {
        self.root.join("lease-v1.lock")
    }

    fn entry_path(&self, memory_id: &str) -> PathBuf {
        self.entries_dir().join(format!("{memory_id}.json"))
    }

    fn ensure_tree(&self) -> Result<()> {
        ensure_private_directory(&self.root)?;
        ensure_private_directory(&self.entries_dir())
    }

    fn lock_exclusive(&self) -> Result<File> {
        self.ensure_tree()?;
        let path = self.lease_path();
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        secure_private_path_permissions(&path)?;
        file.lock_exclusive()
            .with_context(|| format!("failed to lock {}", path.display()))?;
        Ok(file)
    }

    fn remember(&self, statement: &str, source: MemorySourceRefV1) -> Result<DurableMemoryReceipt> {
        let statement = validate_memory_statement(statement)?;
        let _lease = self.lock_exclusive()?;
        let projection = self.load_projection_locked()?;
        let active = self.load_active_locked(&projection)?;
        if let Some(existing) = active
            .values()
            .find(|entry| entry.sidecar.statement == statement)
        {
            return Ok(DurableMemoryReceipt {
                receipt_type: "durable_memory_receipt_v1",
                durable: true,
                created: false,
                memory_id: existing.sidecar.memory_id.clone(),
                logical_memory_id: existing.sidecar.logical_memory_id.clone(),
                version: existing.sidecar.version,
                scope: self.scope,
                workspace_id: self.workspace_id.clone(),
                stored_at_ms: existing.sidecar.created_at_ms,
            });
        }
        ensure!(
            active.len() < MEMORY_SCOPE_MAX_ENTRIES,
            "writable-memory scope reached its explicit entry limit; inspect and forget old entries before adding more"
        );
        self.ensure_journal_exists_locked()?;
        let memory_id = format!("memory-{}", uuid::Uuid::new_v4());
        let created_at_ms = unix_time_ms()?;
        let sidecar = MemorySidecarV1 {
            schema_version: MEMORY_SCHEMA_VERSION,
            memory_id: memory_id.clone(),
            logical_memory_id: memory_id.clone(),
            version: 1,
            scope: self.scope,
            workspace_id: self.workspace_id.clone(),
            content_sha256: sha256_hex(statement.as_bytes()),
            statement,
            trust: "user_asserted".to_owned(),
            validity: "active".to_owned(),
            sensitivity: match self.scope {
                WritableMemoryScope::UserPreference => "user_private",
                WritableMemoryScope::ProjectFact => "repository",
            }
            .to_owned(),
            created_at_ms,
            source: source.clone(),
        };
        validate_sidecar(&sidecar, self.scope, self.workspace_id.as_deref())?;
        let bytes = serde_json::to_vec(&sidecar).context("failed to encode writable memory")?;
        ensure!(
            bytes.len() as u64 <= MEMORY_SIDECAR_MAX_BYTES,
            "writable-memory sidecar exceeds its size limit"
        );
        let sidecar_path = self.entry_path(&memory_id);
        ensure!(
            !sidecar_path.exists(),
            "writable-memory id unexpectedly collides with an existing sidecar"
        );
        if let Err(publish_error) = atomic_publish_private_file(&sidecar_path, &bytes) {
            return match self.remove_unadmitted_sidecar_locked(&sidecar_path) {
                Ok(()) => Err(publish_error),
                Err(cleanup_error) => Err(publish_error).context(format!(
                    "failed to remove an unadmitted writable-memory sidecar: {cleanup_error:#}"
                )),
            };
        }
        let admission_record = MemoryJournalRecordV1 {
            schema_version: MEMORY_SCHEMA_VERSION,
            event_id: format!("memory-event-{}", uuid::Uuid::new_v4()),
            action: MemoryJournalAction::Admitted,
            scope: self.scope,
            workspace_id: self.workspace_id.clone(),
            memory_ids: vec![memory_id.clone()],
            occurred_at_ms: created_at_ms,
            source_session_scope_id: Some(source.session_scope_id),
            source_logical_run_id: Some(source.logical_run_id),
            source_tool_call_id: Some(source.tool_call_id),
        };
        if let Err(append_error) = self.append_record_locked(admission_record) {
            return match self.remove_unadmitted_sidecar_locked(&sidecar_path) {
                Ok(()) => Err(append_error),
                Err(cleanup_error) => Err(append_error).context(format!(
                    "failed to roll back an unadmitted writable-memory sidecar: {cleanup_error:#}"
                )),
            };
        }
        Ok(DurableMemoryReceipt {
            receipt_type: "durable_memory_receipt_v1",
            durable: true,
            created: true,
            memory_id: memory_id.clone(),
            logical_memory_id: memory_id,
            version: 1,
            scope: self.scope,
            workspace_id: self.workspace_id.clone(),
            stored_at_ms: created_at_ms,
        })
    }

    fn inspect(&self, limit: usize) -> Result<Vec<DurableMemoryInspectEntry>> {
        let _lease = self.lock_exclusive()?;
        let projection = self.load_projection_locked()?;
        let mut entries = self
            .load_active_locked(&projection)?
            .into_values()
            .map(|entry| DurableMemoryInspectEntry {
                memory_id: entry.sidecar.memory_id,
                logical_memory_id: entry.sidecar.logical_memory_id,
                version: entry.sidecar.version,
                scope: entry.sidecar.scope,
                statement: entry.sidecar.statement,
                trust: entry.sidecar.trust,
                validity: entry.sidecar.validity,
                created_at_ms: entry.sidecar.created_at_ms,
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| left.memory_id.cmp(&right.memory_id))
        });
        entries.truncate(limit);
        Ok(entries)
    }

    fn find_active(&self, memory_id: &str) -> Result<Option<ProjectedMemory>> {
        validate_opaque_id(memory_id, "memory-")?;
        let _lease = self.lock_exclusive()?;
        let projection = self.load_projection_locked()?;
        Ok(self.load_active_locked(&projection)?.remove(memory_id))
    }

    fn contains(&self, memory_id: &str) -> Result<bool> {
        validate_opaque_id(memory_id, "memory-")?;
        let _lease = self.lock_exclusive()?;
        Ok(self.load_projection_locked()?.contains_key(memory_id))
    }

    fn forget(
        &self,
        memory_id: &str,
        source: MemorySourceRefV1,
    ) -> Result<DurableMemoryForgetReceipt> {
        validate_opaque_id(memory_id, "memory-")?;
        let _lease = self.lock_exclusive()?;
        let mut projection = self.load_projection_locked()?;
        let state = projection
            .get(memory_id)
            .cloned()
            .ok_or_else(|| anyhow!("memory {memory_id} is not known in this scope"))?;
        let completed_at_ms = unix_time_ms()?;
        if state.status == ProjectedMemoryStatus::Active {
            self.append_record_locked(MemoryJournalRecordV1 {
                schema_version: MEMORY_SCHEMA_VERSION,
                event_id: format!("memory-event-{}", uuid::Uuid::new_v4()),
                action: MemoryJournalAction::Tombstoned,
                scope: self.scope,
                workspace_id: self.workspace_id.clone(),
                memory_ids: vec![memory_id.to_owned()],
                occurred_at_ms: completed_at_ms,
                source_session_scope_id: Some(source.session_scope_id.clone()),
                source_logical_run_id: Some(source.logical_run_id.clone()),
                source_tool_call_id: Some(source.tool_call_id.clone()),
            })?;
            projection.get_mut(memory_id).expect("known memory").status =
                ProjectedMemoryStatus::Tombstoned;
        }
        let path = self.entry_path(memory_id);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "writable-memory sidecar target is unsafe"
                );
                fs::remove_file(&path)
                    .with_context(|| format!("failed to delete {}", path.display()))?;
                sync_directory(&self.entries_dir())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if projection
            .get(memory_id)
            .is_some_and(|entry| entry.status != ProjectedMemoryStatus::Deleted)
        {
            self.append_record_locked(MemoryJournalRecordV1 {
                schema_version: MEMORY_SCHEMA_VERSION,
                event_id: format!("memory-event-{}", uuid::Uuid::new_v4()),
                action: MemoryJournalAction::Deleted,
                scope: self.scope,
                workspace_id: self.workspace_id.clone(),
                memory_ids: vec![memory_id.to_owned()],
                occurred_at_ms: completed_at_ms,
                source_session_scope_id: Some(source.session_scope_id),
                source_logical_run_id: Some(source.logical_run_id),
                source_tool_call_id: Some(source.tool_call_id),
            })?;
        }
        Ok(DurableMemoryForgetReceipt {
            receipt_type: "durable_memory_forget_receipt_v1",
            durable: true,
            memory_id: memory_id.to_owned(),
            scope: self.scope,
            tombstoned: true,
            physical_deleted: true,
            prior_provider_context_retracted: false,
            completed_at_ms,
        })
    }

    fn retrieve(&self, query: &str, token_budget: usize) -> Result<Vec<(ProjectedMemory, f32)>> {
        let _lease = self.lock_exclusive()?;
        let projection = self.load_projection_locked()?;
        let active = self.load_active_locked(&projection)?;
        let mut scored = active
            .into_values()
            .filter_map(|entry| {
                let score = match self.scope {
                    WritableMemoryScope::UserPreference => 100.0,
                    WritableMemoryScope::ProjectFact => {
                        memory_relevance(query, &entry.sidecar.statement)
                    }
                };
                (score > 0.0).then_some((entry, score))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|(left, left_score), (right, right_score)| {
            right_score
                .partial_cmp(left_score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| right.sidecar.created_at_ms.cmp(&left.sidecar.created_at_ms))
                .then_with(|| left.sidecar.memory_id.cmp(&right.sidecar.memory_id))
        });
        scored.truncate(match self.scope {
            WritableMemoryScope::UserPreference => MEMORY_CONTEXT_MAX_PREFERENCES,
            WritableMemoryScope::ProjectFact => MEMORY_CONTEXT_MAX_PROJECT_FACTS,
        });
        let mut selected_tokens = 0usize;
        scored.retain(|(entry, _)| {
            let token_cost = estimate_context_token_cost(&memory_context_snippet(entry));
            if selected_tokens.saturating_add(token_cost) > token_budget {
                return false;
            }
            selected_tokens = selected_tokens.saturating_add(token_cost);
            true
        });
        Ok(scored)
    }

    fn load_projection_locked(&self) -> Result<BTreeMap<String, ProjectedMemoryRef>> {
        let path = self.journal_path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BTreeMap::new());
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "writable-memory journal must be a regular non-symlink file"
        );
        ensure!(
            metadata.len() <= MEMORY_JOURNAL_MAX_BYTES,
            "writable-memory journal exceeds its size limit"
        );
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut projection = BTreeMap::<String, ProjectedMemoryRef>::new();
        let mut event_ids = BTreeSet::new();
        for (index, line) in raw.lines().enumerate() {
            ensure!(
                index < MEMORY_JOURNAL_MAX_RECORDS,
                "writable-memory journal exceeds its record limit"
            );
            ensure!(
                !line.trim().is_empty(),
                "writable-memory journal contains a blank record"
            );
            let record: MemoryJournalRecordV1 = serde_json::from_str(line).with_context(|| {
                format!(
                    "failed to decode writable-memory journal line {}",
                    index + 1
                )
            })?;
            record.validate(self.scope, self.workspace_id.as_deref())?;
            ensure!(
                event_ids.insert(record.event_id.clone()),
                "writable-memory journal duplicates an event id"
            );
            match record.action {
                MemoryJournalAction::Admitted => {
                    let memory_id = record.memory_ids[0].clone();
                    let admission_event_id = record.event_id;
                    ensure!(
                        projection
                            .insert(
                                memory_id,
                                ProjectedMemoryRef {
                                    status: ProjectedMemoryStatus::Active,
                                    admission_event_id,
                                },
                            )
                            .is_none(),
                        "writable-memory journal admits one memory more than once"
                    );
                }
                MemoryJournalAction::Tombstoned => {
                    let state = projection.get_mut(&record.memory_ids[0]).ok_or_else(|| {
                        anyhow!("writable-memory tombstone references an unknown memory")
                    })?;
                    ensure!(
                        state.status == ProjectedMemoryStatus::Active,
                        "writable-memory tombstone has an invalid prior state"
                    );
                    state.status = ProjectedMemoryStatus::Tombstoned;
                }
                MemoryJournalAction::Deleted => {
                    let state = projection.get_mut(&record.memory_ids[0]).ok_or_else(|| {
                        anyhow!("writable-memory delete references an unknown memory")
                    })?;
                    ensure!(
                        state.status == ProjectedMemoryStatus::Tombstoned,
                        "writable-memory delete requires a tombstone"
                    );
                    state.status = ProjectedMemoryStatus::Deleted;
                }
            }
        }
        Ok(projection)
    }

    fn load_active_locked(
        &self,
        projection: &BTreeMap<String, ProjectedMemoryRef>,
    ) -> Result<BTreeMap<String, ProjectedMemory>> {
        self.reconcile_orphan_sidecars_locked(projection)?;
        let mut active = BTreeMap::new();
        for (memory_id, state) in projection {
            if state.status != ProjectedMemoryStatus::Active {
                continue;
            }
            let path = self.entry_path(memory_id);
            let metadata = fs::symlink_metadata(&path).with_context(|| {
                format!(
                    "active writable-memory sidecar is missing: {}",
                    path.display()
                )
            })?;
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "active writable-memory sidecar is unsafe"
            );
            ensure!(
                metadata.len() <= MEMORY_SIDECAR_MAX_BYTES,
                "writable-memory sidecar exceeds its size limit"
            );
            let sidecar: MemorySidecarV1 = serde_json::from_slice(&fs::read(&path)?)
                .with_context(|| format!("failed to decode {}", path.display()))?;
            validate_sidecar(&sidecar, self.scope, self.workspace_id.as_deref())?;
            ensure!(
                sidecar.memory_id == *memory_id,
                "writable-memory sidecar identity does not match its lifecycle"
            );
            active.insert(
                memory_id.clone(),
                ProjectedMemory {
                    sidecar,
                    admission_event_id: state.admission_event_id.clone(),
                },
            );
        }
        Ok(active)
    }

    fn ensure_journal_exists_locked(&self) -> Result<()> {
        let path = self.journal_path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "writable-memory journal target is unsafe"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                atomic_publish_private_file(&path, b"")?;
            }
            Err(error) => return Err(error.into()),
        }
        secure_private_path_permissions(&path)
    }

    fn remove_unadmitted_sidecar_locked(&self, path: &Path) -> Result<()> {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                fs::remove_file(path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
                sync_directory(&self.entries_dir())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn reconcile_orphan_sidecars_locked(
        &self,
        projection: &BTreeMap<String, ProjectedMemoryRef>,
    ) -> Result<()> {
        let mut removed = false;
        for entry in fs::read_dir(self.entries_dir())? {
            let entry = entry?;
            let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Some(memory_id) = file_name.strip_suffix(".json") else {
                continue;
            };
            if validate_opaque_id(memory_id, "memory-").is_err()
                || projection.contains_key(memory_id)
            {
                continue;
            }
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "failed to remove unadmitted writable-memory sidecar {}",
                    entry.path().display()
                )
            })?;
            removed = true;
        }
        if removed {
            sync_directory(&self.entries_dir())?;
        }
        Ok(())
    }

    fn append_record_locked(&self, record: MemoryJournalRecordV1) -> Result<()> {
        record.validate(self.scope, self.workspace_id.as_deref())?;
        let path = self.journal_path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("writable-memory journal is missing: {}", path.display()))?;
        ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "writable-memory journal target is unsafe"
        );
        let mut bytes = serde_json::to_vec(&record).context("failed to encode memory event")?;
        bytes.push(b'\n');
        let current = fs::read(&path)?;
        ensure!(
            current.is_empty() || current.ends_with(b"\n"),
            "writable-memory journal has an incomplete trailing record"
        );
        let current_records = current.iter().filter(|byte| **byte == b'\n').count();
        validate_journal_append_bounds(current.len(), current_records, bytes.len())?;
        let mut options = OpenOptions::new();
        options.append(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options
            .open(&path)
            .with_context(|| format!("failed to open {}", path.display()))?;
        secure_private_path_permissions(&path)?;
        let original_len = current.len() as u64;
        if let Err(append_error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
            let rollback = file.set_len(original_len).and_then(|()| file.sync_all());
            return match rollback {
                Ok(()) => Err(append_error.into()),
                Err(rollback_error) => Err(anyhow!(append_error)).context(format!(
                    "failed to roll back a partial writable-memory journal append: {rollback_error}"
                )),
            };
        }
        Ok(())
    }
}

fn validate_journal_append_bounds(
    current_bytes: usize,
    current_records: usize,
    appended_bytes: usize,
) -> Result<()> {
    ensure!(
        current_records < MEMORY_JOURNAL_MAX_RECORDS,
        "writable-memory journal reached its record limit"
    );
    ensure!(
        current_bytes.saturating_add(appended_bytes) <= MEMORY_JOURNAL_MAX_BYTES as usize,
        "writable-memory journal reached its size limit"
    );
    Ok(())
}

/// Runtime-owned writable memory store with one user-global and one workspace-scoped lineage.
#[derive(Debug, Clone)]
pub(crate) struct WritableMemoryStore {
    user_preferences: MemoryScopeStore,
    project_facts: MemoryScopeStore,
}

impl WritableMemoryStore {
    #[must_use]
    pub(crate) fn from_paths(paths: &SigilPaths) -> Self {
        Self {
            user_preferences: MemoryScopeStore::new(
                paths
                    .state_root
                    .join("memory")
                    .join("v1")
                    .join("user-preferences"),
                WritableMemoryScope::UserPreference,
                None,
            ),
            project_facts: MemoryScopeStore::new(
                paths
                    .workspace_state_root
                    .join("memory")
                    .join("v1")
                    .join("project-facts"),
                WritableMemoryScope::ProjectFact,
                Some(paths.workspace_id.clone()),
            ),
        }
    }

    fn scope_store(&self, scope: WritableMemoryScope) -> &MemoryScopeStore {
        match scope {
            WritableMemoryScope::UserPreference => &self.user_preferences,
            WritableMemoryScope::ProjectFact => &self.project_facts,
        }
    }

    fn remember(
        &self,
        scope: WritableMemoryScope,
        statement: &str,
        source: MemoryWriteSource,
    ) -> Result<DurableMemoryReceipt> {
        self.scope_store(scope)
            .remember(statement, source.into_owned()?)
    }

    fn inspect(
        &self,
        scope: Option<WritableMemoryScope>,
        limit: usize,
    ) -> Result<Vec<DurableMemoryInspectEntry>> {
        ensure!(
            (1..=100).contains(&limit),
            "memory inspect limit must be between 1 and 100"
        );
        let mut entries = Vec::new();
        if scope.is_none() || scope == Some(WritableMemoryScope::UserPreference) {
            entries.extend(self.user_preferences.inspect(limit)?);
        }
        if scope.is_none() || scope == Some(WritableMemoryScope::ProjectFact) {
            entries.extend(self.project_facts.inspect(limit)?);
        }
        entries.sort_by(|left, right| {
            right
                .created_at_ms
                .cmp(&left.created_at_ms)
                .then_with(|| left.memory_id.cmp(&right.memory_id))
        });
        entries.truncate(limit);
        Ok(entries)
    }

    fn find_active(&self, memory_id: &str) -> Result<Option<DurableMemoryInspectEntry>> {
        for store in [&self.user_preferences, &self.project_facts] {
            if let Some(entry) = store.find_active(memory_id)? {
                return Ok(Some(DurableMemoryInspectEntry {
                    memory_id: entry.sidecar.memory_id,
                    logical_memory_id: entry.sidecar.logical_memory_id,
                    version: entry.sidecar.version,
                    scope: entry.sidecar.scope,
                    statement: entry.sidecar.statement,
                    trust: entry.sidecar.trust,
                    validity: entry.sidecar.validity,
                    created_at_ms: entry.sidecar.created_at_ms,
                }));
            }
        }
        Ok(None)
    }

    fn known_scope(&self, memory_id: &str) -> Result<Option<WritableMemoryScope>> {
        if self.user_preferences.contains(memory_id)? {
            return Ok(Some(WritableMemoryScope::UserPreference));
        }
        if self.project_facts.contains(memory_id)? {
            return Ok(Some(WritableMemoryScope::ProjectFact));
        }
        Ok(None)
    }

    fn forget(
        &self,
        memory_id: &str,
        source: MemoryWriteSource,
    ) -> Result<DurableMemoryForgetReceipt> {
        let source = source.into_owned()?;
        match self.known_scope(memory_id)? {
            Some(WritableMemoryScope::UserPreference) => {
                self.user_preferences.forget(memory_id, source)
            }
            Some(WritableMemoryScope::ProjectFact) => self.project_facts.forget(memory_id, source),
            None => bail!("memory {memory_id} is not known in user or current-project memory"),
        }
    }

    pub(crate) fn retrieve_context(
        &self,
        query: &str,
    ) -> Result<sigil_kernel::RuntimeContextCandidates> {
        let mut candidates = sigil_kernel::RuntimeContextCandidates::new();
        let mut used_tokens = 0usize;
        for store in [&self.user_preferences, &self.project_facts] {
            let remaining_tokens = MEMORY_CONTEXT_MAX_TOKENS.saturating_sub(used_tokens);
            for (entry, score) in store.retrieve(query, remaining_tokens)? {
                let snippet = memory_context_snippet(&entry);
                let token_cost = estimate_context_token_cost(&snippet);
                used_tokens = used_tokens.saturating_add(token_cost);
                let item_id = format!(
                    "writable-memory:{}:{}:v{}",
                    entry.sidecar.scope.as_str(),
                    entry.sidecar.memory_id,
                    entry.sidecar.version
                );
                candidates.items.push(ContextItem {
                    id: item_id.clone(),
                    source: entry.sidecar.scope.context_source(),
                    source_event_id: Some(entry.admission_event_id.clone()),
                    trust_level: ContextTrustLevel::UserProvided,
                    sensitivity: match entry.sidecar.scope {
                        WritableMemoryScope::UserPreference => ContextSensitivity::UserPrivate,
                        WritableMemoryScope::ProjectFact => ContextSensitivity::Repository,
                    },
                    egress_decision: None,
                    repo_revision: None,
                    token_cost,
                    score: Some(score),
                    score_breakdown: vec![ContextScoreComponent {
                        kind: ContextScoreComponentKind::RetrievalScore,
                        value: score,
                    }],
                    inclusion_reason: ContextInclusionReason::RetrievalHit,
                    body_ref: ContextBodyRef::inline(&snippet),
                });
                candidates.snippets.insert(item_id, snippet);
            }
        }
        Ok(candidates)
    }
}

fn memory_context_snippet(entry: &ProjectedMemory) -> String {
    format!(
        "[durable {} memory; id={}; version={}; trust=user_asserted; validity=active]\n{}",
        entry.sidecar.scope.as_str(),
        entry.sidecar.memory_id,
        entry.sidecar.version,
        entry.sidecar.statement
    )
}

#[derive(Debug, Clone)]
struct MemoryWriteSource {
    session_scope_id: String,
    logical_run_id: String,
    tool_call_id: String,
}

impl MemoryWriteSource {
    fn into_owned(self) -> Result<MemorySourceRefV1> {
        for (label, value) in [
            ("session scope", self.session_scope_id.as_str()),
            ("logical run", self.logical_run_id.as_str()),
            ("tool call", self.tool_call_id.as_str()),
        ] {
            ensure!(
                !value.trim().is_empty() && value.len() <= 256,
                "memory source {label} is invalid"
            );
            ensure!(
                safe_persistence_text(value) == value,
                "memory source {label} is not safe to persist"
            );
        }
        Ok(MemorySourceRefV1 {
            session_scope_id: self.session_scope_id,
            logical_run_id: self.logical_run_id,
            tool_call_id: self.tool_call_id,
        })
    }
}

pub(crate) fn register_writable_memory_tools(
    registry: &mut ToolRegistry,
    store: WritableMemoryStore,
) {
    let store = Arc::new(store);
    registry.register(Arc::new(RememberMemoryTool {
        store: Arc::clone(&store),
        scope: WritableMemoryScope::UserPreference,
    }));
    registry.register(Arc::new(RememberMemoryTool {
        store: Arc::clone(&store),
        scope: WritableMemoryScope::ProjectFact,
    }));
    registry.register(Arc::new(InspectMemoryTool {
        store: Arc::clone(&store),
    }));
    registry.register(Arc::new(ForgetMemoryTool { store }));
}

#[derive(Debug)]
struct RememberMemoryTool {
    store: Arc<WritableMemoryStore>,
    scope: WritableMemoryScope,
}

#[async_trait]
impl Tool for RememberMemoryTool {
    fn spec(&self) -> ToolSpec {
        remember_memory_tool_spec(matches!(self.scope, WritableMemoryScope::ProjectFact))
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::None
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let statement = required_statement(args)?;
        validate_memory_statement(statement)?;
        Ok(memory_write_permission_plan(
            Some(self.scope),
            ToolOperation::RememberMemory,
            "Remember durable memory",
            "Persist one user-confirmed memory in private Sigil state",
        ))
    }

    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        let statement = validate_memory_statement(required_statement(&args)?)?;
        Ok(Some(ToolPreview {
            title: format!("Remember {}", self.scope.as_str()),
            summary: "Store this statement in durable private memory after approval".to_owned(),
            body: statement,
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let statement = required_statement(&args)?.to_owned();
        let source = memory_write_source(&ctx, &call_id)?;
        let store = Arc::clone(&self.store);
        let scope = self.scope;
        let receipt =
            tokio::task::spawn_blocking(move || store.remember(scope, &statement, source))
                .await
                .context("writable-memory task failed")??;
        Ok(ToolResult::ok(
            call_id,
            self.spec().name,
            serde_json::to_string(&receipt)?,
            ToolResultMeta::default(),
        ))
    }
}

#[derive(Debug)]
struct InspectMemoryTool {
    store: Arc<WritableMemoryStore>,
}

#[async_trait]
impl Tool for InspectMemoryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: INSPECT_MEMORY_TOOL_NAME.to_owned(),
            description: "Inspect active durable user preferences and current-project facts, including ids, scope, trust, validity, and creation time. Use this before correcting or forgetting memory.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "enum": ["all", "user_preference", "project_fact"] },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "additionalProperties": false
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Read,
            network_effect: None,
            preview: ToolPreviewCapability::None,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::None
    }

    async fn execute(&self, _ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let scope = parse_scope(args.get("scope").and_then(Value::as_str))?;
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(50) as usize;
        let store = Arc::clone(&self.store);
        let entries = tokio::task::spawn_blocking(move || store.inspect(scope, limit))
            .await
            .context("memory inspection task failed")??;
        Ok(ToolResult::ok(
            call_id,
            INSPECT_MEMORY_TOOL_NAME,
            serde_json::to_string(&json!({
                "schema": "durable_memory_inspect_v1",
                "entries": entries,
                "count": entries.len()
            }))?,
            ToolResultMeta::default(),
        ))
    }
}

#[derive(Debug)]
struct ForgetMemoryTool {
    store: Arc<WritableMemoryStore>,
}

#[async_trait]
impl Tool for ForgetMemoryTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: FORGET_MEMORY_TOOL_NAME.to_owned(),
            description: "Tombstone and physically delete one complete durable-memory lineage by opaque memory id. This stops future Sigil retrieval but cannot retract bytes already sent to a provider or delete independent session/audit evidence.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "memory_id": { "type": "string", "description": "Opaque id returned by a durable memory receipt or inspect_memory." }
                },
                "required": ["memory_id"],
                "additionalProperties": false
            }),
            category: ToolCategory::Custom,
            access: ToolAccess::Write,
            network_effect: None,
            preview: ToolPreviewCapability::Required,
        }
    }

    fn mutation_tracking(&self) -> ToolMutationTracking {
        ToolMutationTracking::None
    }

    fn permission_plan(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolPermissionPlanDraft> {
        let memory_id = required_memory_id(args)?;
        validate_opaque_id(memory_id, "memory-")?;
        Ok(memory_write_permission_plan(
            None,
            ToolOperation::ForgetMemory,
            "Forget durable memory",
            "Tombstone one memory lineage and physically delete its controlled sidecar",
        ))
    }

    async fn preview(&self, _ctx: ToolContext, args: Value) -> Result<Option<ToolPreview>> {
        let memory_id = required_memory_id(&args)?.to_owned();
        let store = Arc::clone(&self.store);
        let lookup_id = memory_id.clone();
        let entry = tokio::task::spawn_blocking(move || {
            let active = store.find_active(&lookup_id)?;
            let known_scope = if active.is_none() {
                store.known_scope(&lookup_id)?
            } else {
                None
            };
            Ok::<_, anyhow::Error>((active, known_scope))
        })
        .await
        .context("memory lookup task failed")??;
        let (title, body) = match entry {
            (Some(entry), _) => (
                format!("Forget {}", entry.memory_id),
                format!(
                    "scope={}\nstatement={}\n\nThis cannot retract prior provider context or independent audit evidence.",
                    entry.scope.as_str(),
                    entry.statement
                ),
            ),
            (None, Some(scope)) => (
                format!("Finish forgetting {memory_id}"),
                format!(
                    "scope={}\nThis memory is already tombstoned or deleted. Confirming finishes any remaining Sigil-controlled physical deletion. This cannot retract prior provider context or independent audit evidence.",
                    scope.as_str()
                ),
            ),
            (None, None) => bail!("memory is not known in user or current-project memory"),
        };
        Ok(Some(ToolPreview {
            title,
            summary: "Stop future retrieval and physically delete the Sigil-controlled memory copy"
                .to_owned(),
            body,
            changed_files: Vec::new(),
            file_diffs: Vec::new(),
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let memory_id = required_memory_id(&args)?.to_owned();
        let source = memory_write_source(&ctx, &call_id)?;
        let store = Arc::clone(&self.store);
        let receipt = tokio::task::spawn_blocking(move || store.forget(&memory_id, source))
            .await
            .context("memory forget task failed")??;
        Ok(ToolResult::ok(
            call_id,
            FORGET_MEMORY_TOOL_NAME,
            serde_json::to_string(&receipt)?,
            ToolResultMeta::default(),
        ))
    }
}

fn memory_write_source(ctx: &ToolContext, call_id: &str) -> Result<MemoryWriteSource> {
    Ok(MemoryWriteSource {
        session_scope_id: ctx
            .session_scope_id()
            .ok_or_else(|| anyhow!("durable memory requires an active session scope"))?
            .to_owned(),
        logical_run_id: ctx
            .logical_run_id()
            .ok_or_else(|| anyhow!("durable memory requires an active logical run"))?
            .to_owned(),
        tool_call_id: call_id.to_owned(),
    })
}

fn memory_write_permission_plan(
    scope: Option<WritableMemoryScope>,
    operation: ToolOperation,
    title: &str,
    detail: &str,
) -> ToolPermissionPlanDraft {
    let scope_label = scope.map_or("any", WritableMemoryScope::as_str);
    let subject = ToolSubject {
        kind: ToolSubjectKind::Other,
        original: format!("durable-memory:{scope_label}"),
        normalized: format!("durable-memory:{scope_label}"),
        canonical_path: None,
        scope: ToolSubjectScope::Unknown,
    };
    let mut semantic_scope = ToolSemanticScope::new("durable_memory", 1);
    semantic_scope
        .qualifiers
        .insert("scope".to_owned(), scope_label.to_owned());
    ToolPermissionPlanDraft {
        access: ToolAccess::Write,
        operation,
        effects: BTreeSet::from([ToolPermissionEffect::FileWrite]),
        subjects: vec![subject],
        analysis: ToolAnalysisStatus::Complete,
        containment: Default::default(),
        semantic_scope: Some(semantic_scope),
        tool_default_mode: Some(ApprovalMode::Ask),
        analysis_bindings: BTreeMap::from([
            ("planner".to_owned(), "durable_memory_v1".to_owned()),
            ("scope".to_owned(), scope_label.to_owned()),
        ]),
        safe_summary: ToolPermissionSummary {
            title: title.to_owned(),
            detail: detail.to_owned(),
            step_count: 1,
            workspace_code_steps: 0,
        },
    }
}

fn required_statement(args: &Value) -> Result<&str> {
    args.get("statement")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("memory statement must be a string"))
}

fn required_memory_id(args: &Value) -> Result<&str> {
    args.get("memory_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("memory_id must be a string"))
}

fn parse_scope(value: Option<&str>) -> Result<Option<WritableMemoryScope>> {
    match value.unwrap_or("all") {
        "all" => Ok(None),
        "user_preference" => Ok(Some(WritableMemoryScope::UserPreference)),
        "project_fact" => Ok(Some(WritableMemoryScope::ProjectFact)),
        _ => bail!("memory scope must be all, user_preference, or project_fact"),
    }
}

fn validate_memory_statement(value: &str) -> Result<String> {
    let value = value.trim();
    ensure!(!value.is_empty(), "memory statement cannot be empty");
    ensure!(
        value.len() <= MEMORY_STATEMENT_MAX_BYTES,
        "memory statement exceeds its byte limit"
    );
    ensure!(
        value.lines().count() <= MEMORY_STATEMENT_MAX_LINES,
        "memory statement exceeds its line limit"
    );
    ensure!(
        safe_persistence_text(value) == value,
        "memory statement contains unsafe control text"
    );
    ensure!(
        !looks_like_secret(value),
        "secret-like or credential material cannot be stored in durable memory"
    );
    Ok(value.to_owned())
}

fn validate_sidecar(
    sidecar: &MemorySidecarV1,
    scope: WritableMemoryScope,
    workspace_id: Option<&str>,
) -> Result<()> {
    ensure!(
        sidecar.schema_version == MEMORY_SCHEMA_VERSION,
        "unsupported writable-memory sidecar schema version"
    );
    validate_opaque_id(&sidecar.memory_id, "memory-")?;
    ensure!(
        sidecar.logical_memory_id == sidecar.memory_id && sidecar.version == 1,
        "writable-memory V1 lineage is invalid"
    );
    ensure!(
        sidecar.scope == scope,
        "writable-memory sidecar scope mismatch"
    );
    ensure!(
        sidecar.workspace_id.as_deref() == workspace_id,
        "writable-memory sidecar workspace mismatch"
    );
    let statement = validate_memory_statement(&sidecar.statement)?;
    ensure!(
        sidecar.content_sha256 == sha256_hex(statement.as_bytes()),
        "writable-memory sidecar content digest mismatch"
    );
    ensure!(
        sidecar.trust == "user_asserted",
        "unsupported memory trust state"
    );
    ensure!(
        sidecar.validity == "active",
        "unsupported memory validity state"
    );
    let expected_sensitivity = match scope {
        WritableMemoryScope::UserPreference => "user_private",
        WritableMemoryScope::ProjectFact => "repository",
    };
    ensure!(
        sidecar.sensitivity == expected_sensitivity,
        "writable-memory sidecar sensitivity mismatch"
    );
    ensure!(
        sidecar.created_at_ms > 0,
        "writable-memory creation time is invalid"
    );
    source_ref_validate(&sidecar.source)
}

fn source_ref_validate(source: &MemorySourceRefV1) -> Result<()> {
    validate_optional_ref(Some(&source.session_scope_id), "session scope")?;
    validate_optional_ref(Some(&source.logical_run_id), "logical run")?;
    validate_optional_ref(Some(&source.tool_call_id), "tool call")
}

fn validate_optional_ref(value: Option<&str>, label: &str) -> Result<()> {
    if let Some(value) = value {
        ensure!(
            !value.trim().is_empty() && value.len() <= 256,
            "writable-memory {label} is invalid"
        );
        ensure!(
            safe_persistence_text(value) == value,
            "writable-memory {label} is unsafe"
        );
    }
    Ok(())
}

fn validate_opaque_id(value: &str, prefix: &str) -> Result<()> {
    let uuid = value
        .strip_prefix(prefix)
        .ok_or_else(|| anyhow!("writable-memory id has an invalid prefix"))?;
    uuid::Uuid::parse_str(uuid).context("writable-memory id is not an opaque UUID")?;
    Ok(())
}

fn ensure_private_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "writable-memory path must be a regular directory: {}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            fs::create_dir(path).with_context(|| format!("failed to create {}", path.display()))?;
        }
        Err(error) => return Err(error.into()),
    }
    secure_private_path_permissions(path)
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("failed to open directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync directory {}", path.display()))
}

fn unix_time_ms() -> Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX))
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn looks_like_secret(value: &str) -> bool {
    let lower = value.to_lowercase();
    if lower.contains("-----begin private key-----")
        || lower.contains("-----begin rsa private key-----")
        || lower.contains("-----begin openssh private key-----")
    {
        return true;
    }
    if value
        .split(|character: char| {
            !character.is_ascii_alphanumeric()
                && character != '-'
                && character != '_'
                && character != '+'
                && character != '/'
                && character != '='
        })
        .any(token_looks_like_credential)
    {
        return true;
    }
    value.lines().any(|line| {
        let Some((key, raw_value)) = line.split_once(['=', ':']) else {
            return false;
        };
        let key = key.trim().to_lowercase();
        let raw_value = raw_value.trim().trim_matches(['\'', '"']);
        !raw_value.is_empty()
            && raw_value.len() >= 8
            && [
                "password",
                "passwd",
                "api_key",
                "apikey",
                "access_token",
                "auth_token",
                "authorization",
                "client_secret",
                "private_key",
                "refresh_token",
                "secret",
                "token",
            ]
            .iter()
            .any(|candidate| key.contains(candidate))
    })
}

fn token_looks_like_credential(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let known_prefix = [
        "sk-",
        "ghp_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "xoxa-",
        "xoxr-",
        "xoxs-",
        "akia",
        "aiza",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix));
    (known_prefix && token.len() >= 20) || looks_like_high_entropy_token(token)
}

fn looks_like_high_entropy_token(token: &str) -> bool {
    if !(32..=512).contains(&token.len()) {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    if ["example", "placeholder", "redacted", "changeme"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return false;
    }
    if !token
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_/+=".contains(character))
    {
        return false;
    }
    let has_lower = token
        .chars()
        .any(|character| character.is_ascii_lowercase());
    let has_upper = token
        .chars()
        .any(|character| character.is_ascii_uppercase());
    let has_digit = token.chars().any(|character| character.is_ascii_digit());
    let has_symbol = token.chars().any(|character| "-_/+=".contains(character));
    let class_count = [has_lower, has_upper, has_digit, has_symbol]
        .into_iter()
        .filter(|present| *present)
        .count();
    let unique_count = token.bytes().collect::<BTreeSet<_>>().len();
    (class_count >= 3 && unique_count >= 12)
        || (token.len() >= 48
            && token.bytes().all(|byte| byte.is_ascii_hexdigit())
            && unique_count >= 10)
}

fn memory_relevance(query: &str, statement: &str) -> f32 {
    let query_terms = memory_terms(query);
    if query_terms.is_empty() {
        return 0.0;
    }
    let statement_terms = memory_terms(statement);
    let overlap = query_terms.intersection(&statement_terms).count();
    if overlap == 0 {
        return 0.0;
    }
    overlap as f32 / query_terms.len().max(1) as f32
}

fn memory_terms(value: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    let mut ascii = String::new();
    for character in value.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            ascii.push(character);
            continue;
        }
        if !ascii.is_empty() {
            if ascii.len() >= 2 {
                terms.insert(std::mem::take(&mut ascii));
            } else {
                ascii.clear();
            }
        }
        if !character.is_ascii() && !character.is_whitespace() && !character.is_ascii_punctuation()
        {
            terms.insert(character.to_string());
        }
    }
    if ascii.len() >= 2 {
        terms.insert(ascii);
    }
    terms
}

#[cfg(test)]
#[path = "tests/writable_memory_tests.rs"]
mod tests;
