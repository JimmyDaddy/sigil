use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::{digest_serializable, sync_directory};

pub const LOCAL_SESSION_LIFECYCLE_JOURNAL_SCHEMA_VERSION: u16 = 1;
const MAX_LIFECYCLE_JOURNAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_LIFECYCLE_JOURNAL_RECORDS: usize = 200_000;

/// Source/export binding retained without copying transcript or raw external destination paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionExportJournalBinding {
    pub source_session_ref: sigil_kernel::SessionRef,
    pub source_session_id: String,
    pub source_content_sha256: String,
    pub destination_file_name: String,
    pub destination_path_sha256: String,
    pub artifact_payload_sha256: String,
    pub message_count: usize,
}

/// Exact source file binding used by delete preview/apply and recovery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionDeleteJournalBinding {
    pub source_session_ref: sigil_kernel::SessionRef,
    pub source_session_id: String,
    pub source_content_sha256: String,
    pub source_bytes: u64,
    pub source_modified_at_unix_ms: u64,
    pub preview_digest: String,
}

/// Typed append-only lifecycle event. Payloads never contain transcript text or raw external
/// destination paths.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "event_type", content = "payload")]
pub enum LocalSessionLifecycleEvent {
    ExportPlanned(LocalSessionExportJournalBinding),
    ExportCompleted(LocalSessionExportJournalBinding),
    DeletePlanned(LocalSessionDeleteJournalBinding),
    DeleteCompleted(LocalSessionDeleteJournalBinding),
}

/// One strict-sequence, previous-hash-linked workspace lifecycle record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct LocalSessionLifecycleRecord {
    pub schema_version: u16,
    pub sequence: u64,
    pub record_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_record_sha256: Option<String>,
    pub operation_id: String,
    pub recorded_at_unix_ms: u64,
    pub event: LocalSessionLifecycleEvent,
    pub record_sha256: String,
}

impl LocalSessionLifecycleRecord {
    fn validate(&self, expected_sequence: u64, expected_previous: Option<&str>) -> Result<()> {
        if self.schema_version != LOCAL_SESSION_LIFECYCLE_JOURNAL_SCHEMA_VERSION {
            bail!("unsupported local session lifecycle journal schema version");
        }
        if self.sequence != expected_sequence {
            bail!("local session lifecycle journal sequence is not contiguous");
        }
        if self.previous_record_sha256.as_deref() != expected_previous {
            bail!("local session lifecycle journal previous hash does not match");
        }
        if self.record_id.trim().is_empty() || self.operation_id.trim().is_empty() {
            bail!("local session lifecycle journal identity is empty");
        }
        let expected = record_digest(
            self.schema_version,
            self.sequence,
            &self.record_id,
            self.previous_record_sha256.as_deref(),
            &self.operation_id,
            self.recorded_at_unix_ms,
            &self.event,
        )?;
        if self.record_sha256 != expected {
            bail!("local session lifecycle journal record hash does not match");
        }
        Ok(())
    }
}

pub(super) struct LocalSessionLifecycleJournal {
    path: PathBuf,
}

impl LocalSessionLifecycleJournal {
    pub(super) fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub(super) fn read_records(&self) -> Result<Vec<LocalSessionLifecycleRecord>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        reject_symlink(&self.path, "lifecycle journal")?;
        let mut file = File::open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        file.try_lock_shared().with_context(|| {
            format!(
                "local session lifecycle journal is busy: {}",
                self.path.display()
            )
        })?;
        read_records_from_file(&mut file, &self.path)
    }

    pub(super) fn append(
        &self,
        operation_id: &str,
        recorded_at_unix_ms: u64,
        event: LocalSessionLifecycleEvent,
    ) -> Result<LocalSessionLifecycleRecord> {
        if operation_id.trim().is_empty() || operation_id.len() > 256 {
            bail!("local session lifecycle operation id is invalid");
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow!("lifecycle journal path has no parent"))?;
        ensure_real_directory(parent)?;
        if self.path.exists() {
            reject_symlink(&self.path, "lifecycle journal")?;
        }
        let lease_path = writer_lease_path(&self.path);
        if lease_path.exists() {
            reject_symlink(&lease_path, "lifecycle journal writer lease")?;
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
                "local session lifecycle journal writer is busy: {}",
                self.path.display()
            )
        })?;

        let existed = self.path.exists();
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open {}", self.path.display()))?;
        file.try_lock().with_context(|| {
            format!(
                "local session lifecycle journal data file is busy: {}",
                self.path.display()
            )
        })?;
        let records = read_records_from_file(&mut file, &self.path)?;
        if records.len() >= MAX_LIFECYCLE_JOURNAL_RECORDS {
            bail!("local session lifecycle journal record capacity is exhausted");
        }
        validate_operation_transition(&records, operation_id, &event)?;
        let sequence = records
            .last()
            .map_or(1, |record| record.sequence.saturating_add(1));
        let previous_record_sha256 = records.last().map(|record| record.record_sha256.clone());
        let record_id = format!("lifecycle:{}", uuid::Uuid::new_v4());
        let record_sha256 = record_digest(
            LOCAL_SESSION_LIFECYCLE_JOURNAL_SCHEMA_VERSION,
            sequence,
            &record_id,
            previous_record_sha256.as_deref(),
            operation_id,
            recorded_at_unix_ms,
            &event,
        )?;
        let record = LocalSessionLifecycleRecord {
            schema_version: LOCAL_SESSION_LIFECYCLE_JOURNAL_SCHEMA_VERSION,
            sequence,
            record_id,
            previous_record_sha256,
            operation_id: operation_id.to_owned(),
            recorded_at_unix_ms,
            event,
            record_sha256,
        };
        let mut bytes = serde_json::to_vec(&record)
            .context("failed to serialize local session lifecycle record")?;
        bytes.push(b'\n');
        let current_bytes = file
            .metadata()
            .with_context(|| format!("failed to inspect {}", self.path.display()))?
            .len();
        if current_bytes.saturating_add(bytes.len() as u64) > MAX_LIFECYCLE_JOURNAL_BYTES {
            bail!("local session lifecycle journal byte capacity is exhausted");
        }
        file.seek(SeekFrom::End(0))
            .with_context(|| format!("failed to seek {}", self.path.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("failed to append {}", self.path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync {}", self.path.display()))?;
        if !existed {
            sync_directory(parent)?;
        }
        Ok(record)
    }
}

fn read_records_from_file(
    file: &mut File,
    path: &Path,
) -> Result<Vec<LocalSessionLifecycleRecord>> {
    let size = file
        .metadata()
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .len();
    if size > MAX_LIFECYCLE_JOURNAL_BYTES {
        bail!("local session lifecycle journal exceeds byte capacity");
    }
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("failed to seek {}", path.display()))?;
    let mut bytes = Vec::with_capacity(size.try_into().unwrap_or(0));
    file.take(MAX_LIFECYCLE_JOURNAL_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() as u64 > MAX_LIFECYCLE_JOURNAL_BYTES {
        bail!("local session lifecycle journal grew beyond byte capacity");
    }
    if !bytes.is_empty() && !bytes.ends_with(b"\n") {
        bail!("local session lifecycle journal has an incomplete tail record");
    }
    let mut records = Vec::new();
    let lines = bytes.split(|byte| *byte == b'\n').collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            if index.saturating_add(1) != lines.len() {
                bail!("local session lifecycle journal contains a blank record");
            }
            continue;
        }
        if records.len() >= MAX_LIFECYCLE_JOURNAL_RECORDS {
            bail!("local session lifecycle journal record capacity is exhausted");
        }
        let record =
            serde_json::from_slice::<LocalSessionLifecycleRecord>(line).with_context(|| {
                format!(
                    "failed to decode local session lifecycle record on line {} in {}",
                    index.saturating_add(1),
                    path.display()
                )
            })?;
        let expected_sequence = records.len() as u64 + 1;
        let expected_previous = records
            .last()
            .map(|record: &LocalSessionLifecycleRecord| record.record_sha256.as_str());
        record.validate(expected_sequence, expected_previous)?;
        records.push(record);
    }
    Ok(records)
}

fn validate_operation_transition(
    records: &[LocalSessionLifecycleRecord],
    operation_id: &str,
    event: &LocalSessionLifecycleEvent,
) -> Result<()> {
    let prior = records
        .iter()
        .filter(|record| record.operation_id == operation_id)
        .collect::<Vec<_>>();
    match event {
        LocalSessionLifecycleEvent::ExportPlanned(_)
        | LocalSessionLifecycleEvent::DeletePlanned(_) => {
            if !prior.is_empty() {
                bail!("local session lifecycle operation is already recorded");
            }
        }
        LocalSessionLifecycleEvent::ExportCompleted(binding) => match prior.as_slice() {
            [record]
                if matches!(
                    &record.event,
                    LocalSessionLifecycleEvent::ExportPlanned(planned) if planned == binding
                ) => {}
            _ => bail!("export completion requires one exact planned binding"),
        },
        LocalSessionLifecycleEvent::DeleteCompleted(binding) => match prior.as_slice() {
            [record]
                if matches!(
                    &record.event,
                    LocalSessionLifecycleEvent::DeletePlanned(planned) if planned == binding
                ) => {}
            _ => bail!("delete completion requires one exact planned binding"),
        },
    }
    Ok(())
}

fn record_digest(
    schema_version: u16,
    sequence: u64,
    record_id: &str,
    previous_record_sha256: Option<&str>,
    operation_id: &str,
    recorded_at_unix_ms: u64,
    event: &LocalSessionLifecycleEvent,
) -> Result<String> {
    digest_serializable(&(
        schema_version,
        sequence,
        record_id,
        previous_record_sha256,
        operation_id,
        recorded_at_unix_ms,
        event,
    ))
}

fn ensure_real_directory(path: &Path) -> Result<()> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {}", path.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("lifecycle journal parent must be a real directory");
        }
    } else {
        fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    }
    Ok(())
}

fn reject_symlink(path: &Path, label: &str) -> Result<()> {
    if fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .file_type()
        .is_symlink()
    {
        bail!("{label} must not be a symlink");
    }
    Ok(())
}

fn writer_lease_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("session-lifecycle-v1.jsonl");
    path.with_file_name(format!("{name}.writer-lock"))
}
