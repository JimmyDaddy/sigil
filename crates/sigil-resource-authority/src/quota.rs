//! RFC-0071 section 10.9: hierarchical quota, atomic reservation and truthful enforcement.
//!
//! Reservation is atomic against concurrent acquirers; workspace cap never overcommits; and the
//! quota receipt is truthful: reservation accounting never pretends to be a backend hard quota.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sigil_kernel::resource::{CanonicalHash, ResourceQuotaClassV1, ResourceQuotaProfileV1};

/// One reservation epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuotaReservationV1 {
    pub reservation_epoch: u64,
    pub reserved_bytes: u64,
    pub reserved_entries: u64,
}

/// Closed quota error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuotaErrorV1 {
    #[error(
        "per-class reservation exceeds the profile maximum bytes: reserved={reserved} max={max}"
    )]
    ReservationExceeded {
        class: &'static str,
        reserved: u64,
        max: u64,
    },
    #[error(
        "per-class reservation exceeds the profile maximum entries: reserved={reserved} max={max}"
    )]
    EntryExceeded {
        class: &'static str,
        reserved: u64,
        max: u64,
    },
    #[error("workspace hard cap would overcommit: used={used} incoming={incoming} cap={cap}")]
    WorkspaceOvercommit { used: u64, incoming: u64, cap: u64 },
    #[error("borrowed accounting profile claims hard runtime enforcement")]
    BorrowedClaimsEnforcement,
    #[error("quota owner already has an active reservation")]
    OwnerAlreadyReserved,
    #[error("durable quota journal failure: {0}")]
    Journal(String),
}

#[derive(Debug, Clone)]
struct ActiveReservationV1 {
    owner_key: String,
    class: ResourceQuotaClassV1,
    reserved_bytes: u64,
    reserved_entries: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum QuotaJournalEventV1 {
    Reserved {
        reservation_epoch: u64,
        #[serde(default)]
        owner_key: String,
        profile: ResourceQuotaProfileV1,
        reserved_bytes: u64,
        reserved_entries: u64,
    },
    Released {
        reservation_epoch: u64,
        #[serde(default)]
        owner_key: String,
        class: ResourceQuotaClassV1,
        reserved_bytes: u64,
        reserved_entries: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QuotaJournalRecordV1 {
    sequence: u64,
    previous_hash: Option<CanonicalHash>,
    event: QuotaJournalEventV1,
    record_hash: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct QuotaJournalSnapshotV1 {
    schema_version: u32,
    workspace_cap: u64,
    records: Vec<QuotaJournalRecordV1>,
}

const QUOTA_JOURNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
struct QuotaJournalV1 {
    path: PathBuf,
    workspace_cap: u64,
    records: Vec<QuotaJournalRecordV1>,
    poisoned: bool,
}

impl QuotaJournalV1 {
    fn open(
        path: impl Into<PathBuf>,
        workspace_cap: u64,
        authorized_previous_cap: Option<u64>,
    ) -> Result<(Self, QuotaBookV1), QuotaErrorV1> {
        let path = path.into();
        let parent = path
            .parent()
            .ok_or_else(|| QuotaErrorV1::Journal("journal has no parent directory".to_owned()))?;
        fs::create_dir_all(parent).map_err(quota_io_error)?;
        secure_quota_path(parent)?;
        if path.exists() {
            let metadata = fs::symlink_metadata(&path).map_err(quota_io_error)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(QuotaErrorV1::Journal(
                    "journal path is not a regular file".to_owned(),
                ));
            }
            secure_quota_path(&path)?;
            let snapshot: QuotaJournalSnapshotV1 =
                serde_json::from_slice(&fs::read(&path).map_err(quota_io_error)?)
                    .map_err(|error| QuotaErrorV1::Journal(format!("invalid journal: {error}")))?;
            if snapshot.schema_version != QUOTA_JOURNAL_SCHEMA_VERSION {
                return Err(QuotaErrorV1::Journal(
                    "unsupported quota journal schema".to_owned(),
                ));
            }
            let authorized_migration = authorized_previous_cap.is_some_and(|previous| {
                snapshot.workspace_cap == previous && workspace_cap > previous
            });
            if snapshot.workspace_cap != workspace_cap && !authorized_migration {
                return Err(QuotaErrorV1::Journal(
                    "quota journal workspace cap mismatch".to_owned(),
                ));
            }
            let mut book = QuotaBookV1::new(workspace_cap);
            verify_and_replay_records(&mut book, &snapshot.records)?;
            let expected_predecessor = snapshot.clone();
            let journal = Self {
                path,
                workspace_cap,
                records: snapshot.records,
                poisoned: false,
            };
            if authorized_migration {
                journal.persist(Some(&expected_predecessor))?;
            }
            Ok((journal, book))
        } else {
            let journal = Self {
                path,
                workspace_cap,
                records: Vec::new(),
                poisoned: false,
            };
            journal.persist(None)?;
            Ok((journal, QuotaBookV1::new(workspace_cap)))
        }
    }

    fn append(&mut self, event: QuotaJournalEventV1) -> Result<(), QuotaErrorV1> {
        if self.poisoned {
            return Err(QuotaErrorV1::Journal(
                "journal is poisoned after an uncertain durability failure".to_owned(),
            ));
        }
        let record = QuotaJournalRecordV1 {
            sequence: self.records.len() as u64 + 1,
            previous_hash: self.records.last().map(|record| record.record_hash),
            record_hash: quota_record_hash(
                self.records.len() as u64 + 1,
                self.records.last().map(|record| record.record_hash),
                &event,
            )?,
            event,
        };
        self.records.push(record);
        let expected_predecessor = QuotaJournalSnapshotV1 {
            schema_version: QUOTA_JOURNAL_SCHEMA_VERSION,
            workspace_cap: self.workspace_cap,
            records: self.records[..self.records.len().saturating_sub(1)].to_vec(),
        };
        if let Err(error) = self.persist(Some(&expected_predecessor)) {
            self.records.pop();
            if matches!(error, QuotaErrorV1::Journal(ref message) if message.contains("uncertain"))
            {
                self.poisoned = true;
            }
            return Err(error);
        }
        Ok(())
    }

    fn persist(
        &self,
        expected_predecessor: Option<&QuotaJournalSnapshotV1>,
    ) -> Result<(), QuotaErrorV1> {
        let _writer_lock =
            crate::durable_snapshot::open_owner_only_snapshot_writer_lock(&self.path)
                .map_err(quota_io_error)?;
        match expected_predecessor {
            Some(expected) => {
                let metadata = fs::symlink_metadata(&self.path).map_err(quota_io_error)?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(QuotaErrorV1::Journal(
                        "quota journal path is not a regular file".to_owned(),
                    ));
                }
                let current: QuotaJournalSnapshotV1 =
                    serde_json::from_slice(&fs::read(&self.path).map_err(quota_io_error)?)
                        .map_err(|error| {
                            QuotaErrorV1::Journal(format!("invalid journal: {error}"))
                        })?;
                if current != *expected {
                    return Err(QuotaErrorV1::Journal(
                        "quota journal append precondition mismatch".to_owned(),
                    ));
                }
            }
            None if self.path.exists() => {
                return Err(QuotaErrorV1::Journal(
                    "quota journal create precondition mismatch".to_owned(),
                ));
            }
            None => {}
        }
        let snapshot = QuotaJournalSnapshotV1 {
            schema_version: QUOTA_JOURNAL_SCHEMA_VERSION,
            workspace_cap: self.workspace_cap,
            records: self.records.clone(),
        };
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|error| QuotaErrorV1::Journal(format!("serialize journal: {error}")))?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| QuotaErrorV1::Journal("journal has no parent directory".to_owned()))?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| QuotaErrorV1::Journal(format!("clock failure: {error}")))?
            .as_nanos();
        let temp_path = parent.join(format!(
            ".{}.quota-{}.tmp",
            self.path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("journal"),
            nonce
        ));
        let write_result = (|| -> Result<(), QuotaErrorV1> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp_path).map_err(quota_io_error)?;
            file.write_all(&bytes).map_err(quota_io_error)?;
            file.sync_all().map_err(quota_io_error)?;
            fs::rename(&temp_path, &self.path).map_err(quota_io_error)?;
            #[cfg(unix)]
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(quota_io_error)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }
}

fn quota_io_error(error: std::io::Error) -> QuotaErrorV1 {
    QuotaErrorV1::Journal(error.to_string())
}

fn secure_quota_path(path: &std::path::Path) -> Result<(), QuotaErrorV1> {
    sigil_kernel::secure_private_path_permissions(path)
        .map_err(|error| QuotaErrorV1::Journal(error.to_string()))
}

fn quota_record_hash(
    sequence: u64,
    previous_hash: Option<CanonicalHash>,
    event: &QuotaJournalEventV1,
) -> Result<CanonicalHash, QuotaErrorV1> {
    #[derive(Serialize)]
    struct HashInput<'a> {
        sequence: u64,
        previous_hash: Option<CanonicalHash>,
        event: &'a QuotaJournalEventV1,
    }
    let input = serde_json::to_vec(&HashInput {
        sequence,
        previous_hash,
        event,
    })
    .map_err(|error| QuotaErrorV1::Journal(format!("hash journal record: {error}")))?;
    let mut hasher = Sha256::new();
    hasher.update(input);
    Ok(CanonicalHash::from_bytes(hasher.finalize().into()))
}

fn verify_and_replay_records(
    book: &mut QuotaBookV1,
    records: &[QuotaJournalRecordV1],
) -> Result<(), QuotaErrorV1> {
    let mut previous_hash = None;
    for (index, record) in records.iter().enumerate() {
        let expected_sequence = index as u64 + 1;
        if record.sequence != expected_sequence || record.previous_hash != previous_hash {
            return Err(QuotaErrorV1::Journal(
                "quota journal sequence or chain mismatch".to_owned(),
            ));
        }
        let expected_hash =
            quota_record_hash(record.sequence, record.previous_hash, &record.event)?;
        if record.record_hash != expected_hash {
            return Err(QuotaErrorV1::Journal(
                "quota journal record hash mismatch".to_owned(),
            ));
        }
        match &record.event {
            QuotaJournalEventV1::Reserved {
                reservation_epoch,
                owner_key,
                profile,
                reserved_bytes,
                reserved_entries,
            } => book.apply_replayed_reservation(
                *reservation_epoch,
                owner_key,
                profile,
                *reserved_bytes,
                *reserved_entries,
            )?,
            QuotaJournalEventV1::Released {
                reservation_epoch,
                owner_key,
                class,
                reserved_bytes,
                reserved_entries,
            } => book.apply_replayed_release(
                *reservation_epoch,
                owner_key,
                *class,
                *reserved_bytes,
                *reserved_entries,
            )?,
        }
        previous_hash = Some(record.record_hash);
    }
    Ok(())
}

/// Owner-local quota bookkeeping (single writer per shard).
#[derive(Debug, Default)]
pub struct QuotaBookV1 {
    per_class_bytes: BTreeMap<ResourceQuotaClassV1, u64>,
    per_class_entries: BTreeMap<ResourceQuotaClassV1, u64>,
    workspace_bytes: u64,
    workspace_cap: u64,
    reservation_epoch: u64,
    active_reservations: BTreeMap<u64, ActiveReservationV1>,
    journal: Option<QuotaJournalV1>,
}

impl QuotaBookV1 {
    pub const fn new(workspace_cap: u64) -> Self {
        Self {
            per_class_bytes: std::collections::BTreeMap::new(),
            per_class_entries: std::collections::BTreeMap::new(),
            workspace_bytes: 0,
            workspace_cap,
            reservation_epoch: 0,
            active_reservations: BTreeMap::new(),
            journal: None,
        }
    }

    /// Opens a quota book backed by an owner-only, hash-chained durable journal.
    pub fn open(path: impl Into<PathBuf>, workspace_cap: u64) -> Result<Self, QuotaErrorV1> {
        let (journal, mut book) = QuotaJournalV1::open(path, workspace_cap, None)?;
        book.journal = Some(journal);
        Ok(book)
    }

    /// Opens a durable book while authorizing one exact monotonic policy-cap migration.
    /// Any other cap drift remains fail-closed, including a tampered or decreased cap.
    pub fn open_with_previous_cap(
        path: impl Into<PathBuf>,
        workspace_cap: u64,
        authorized_previous_cap: u64,
    ) -> Result<Self, QuotaErrorV1> {
        let (journal, mut book) =
            QuotaJournalV1::open(path, workspace_cap, Some(authorized_previous_cap))?;
        book.journal = Some(journal);
        Ok(book)
    }

    /// Reopens an existing journal using its bound workspace cap. Callers that accept a new
    /// policy must still use [`Self::open`], which rejects cap drift explicitly.
    pub fn open_existing(path: impl Into<PathBuf>) -> Result<Self, QuotaErrorV1> {
        let path = path.into();
        let metadata = fs::symlink_metadata(&path).map_err(quota_io_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(QuotaErrorV1::Journal(
                "journal path is not a regular file".to_owned(),
            ));
        }
        let bytes = fs::read(&path).map_err(quota_io_error)?;
        let snapshot: QuotaJournalSnapshotV1 = serde_json::from_slice(&bytes)
            .map_err(|error| QuotaErrorV1::Journal(format!("invalid journal: {error}")))?;
        Self::open(path, snapshot.workspace_cap)
    }

    #[must_use]
    pub fn is_durable(&self) -> bool {
        self.journal.is_some()
    }

    /// Atomically reserves bytes/entries under one profile. No mutation occurs on failure.
    pub fn reserve(
        &mut self,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<QuotaReservationV1, QuotaErrorV1> {
        self.reserve_owned("", profile, bytes, entries)
    }

    /// Reserves quota for one stable owner key. The owner key is journaled so replay can
    /// reattach the reservation to the same physical owner after restart.
    pub fn reserve_owned(
        &mut self,
        owner_key: impl Into<String>,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<QuotaReservationV1, QuotaErrorV1> {
        let owner_key = owner_key.into();
        if !owner_key.is_empty()
            && self
                .active_reservations
                .values()
                .any(|reservation| reservation.owner_key == owner_key)
        {
            return Err(QuotaErrorV1::OwnerAlreadyReserved);
        }
        if profile.hard_runtime_enforcement_required
            && profile.class == ResourceQuotaClassV1::BorrowedAccountingOnly
        {
            return Err(QuotaErrorV1::BorrowedClaimsEnforcement);
        }
        let class_name = quota_class_label(profile.class);
        let used_bytes = *self.per_class_bytes.get(&profile.class).unwrap_or(&0);
        let used_entries = *self.per_class_entries.get(&profile.class).unwrap_or(&0);
        if used_bytes.saturating_add(bytes) > profile.max_bytes {
            return Err(QuotaErrorV1::ReservationExceeded {
                class: class_name,
                reserved: used_bytes.saturating_add(bytes),
                max: profile.max_bytes,
            });
        }
        if used_entries.saturating_add(entries) > profile.max_entries {
            return Err(QuotaErrorV1::EntryExceeded {
                class: class_name,
                reserved: used_entries.saturating_add(entries),
                max: profile.max_entries,
            });
        }
        if self.workspace_bytes.saturating_add(bytes) > self.workspace_cap {
            return Err(QuotaErrorV1::WorkspaceOvercommit {
                used: self.workspace_bytes,
                incoming: bytes,
                cap: self.workspace_cap,
            });
        }
        let reservation_epoch = self.reservation_epoch.saturating_add(1);
        if let Some(journal) = self.journal.as_mut() {
            journal.append(QuotaJournalEventV1::Reserved {
                reservation_epoch,
                owner_key: owner_key.clone(),
                profile: profile.clone(),
                reserved_bytes: bytes,
                reserved_entries: entries,
            })?;
        }
        self.per_class_bytes
            .insert(profile.class, used_bytes.saturating_add(bytes));
        self.per_class_entries
            .insert(profile.class, used_entries.saturating_add(entries));
        self.workspace_bytes = self.workspace_bytes.saturating_add(bytes);
        self.reservation_epoch = reservation_epoch;
        self.active_reservations.insert(
            reservation_epoch,
            ActiveReservationV1 {
                owner_key,
                class: profile.class,
                reserved_bytes: bytes,
                reserved_entries: entries,
            },
        );
        Ok(QuotaReservationV1 {
            reservation_epoch,
            reserved_bytes: bytes,
            reserved_entries: entries,
        })
    }

    /// Reconciles one owner's current measured usage. A repeated call with identical facts is
    /// idempotent; a changed measurement settles the prior reservation before reserving the new
    /// frontier, and both events are durable when this book is journal-backed.
    pub fn reconcile_owned(
        &mut self,
        owner_key: impl Into<String>,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<QuotaReservationV1, QuotaErrorV1> {
        let owner_key = owner_key.into();
        if let Some((epoch, active)) = self
            .active_reservations
            .iter()
            .find(|(_, reservation)| reservation.owner_key == owner_key)
            .map(|(epoch, reservation)| (*epoch, reservation.clone()))
        {
            if active.class == profile.class
                && active.reserved_bytes == bytes
                && active.reserved_entries == entries
            {
                return Ok(QuotaReservationV1 {
                    reservation_epoch: epoch,
                    reserved_bytes: bytes,
                    reserved_entries: entries,
                });
            }
            self.release_active(epoch, active)?;
        }
        self.reserve_owned(owner_key, profile, bytes, entries)
    }

    /// Returns the current replayed reservation for an owner, if any.
    #[must_use]
    pub fn reservation_for_owner(&self, owner_key: &str) -> Option<QuotaReservationV1> {
        self.active_reservations
            .iter()
            .find(|(_, reservation)| reservation.owner_key == owner_key)
            .map(|(epoch, reservation)| QuotaReservationV1 {
                reservation_epoch: *epoch,
                reserved_bytes: reservation.reserved_bytes,
                reserved_entries: reservation.reserved_entries,
            })
    }

    /// Returns the durable owner identities currently holding reservations. Domain authorities
    /// use this only during restart reconciliation to release reservations whose corresponding
    /// admission never became durable.
    #[must_use]
    pub fn active_owner_keys(&self) -> std::collections::BTreeSet<String> {
        self.active_reservations
            .values()
            .map(|reservation| reservation.owner_key.clone())
            .collect()
    }

    /// Settles the active reservation for one owner. Unknown owners are idempotent.
    pub fn release_owner(&mut self, owner_key: &str) -> Result<(), QuotaErrorV1> {
        let Some((epoch, active)) = self
            .active_reservations
            .iter()
            .find(|(_, reservation)| reservation.owner_key == owner_key)
            .map(|(epoch, reservation)| (*epoch, reservation.clone()))
        else {
            return Ok(());
        };
        self.release_active(epoch, active)
    }

    /// Releases a reservation (settlement). Idempotent on unknown or mismatched epochs.
    pub fn release(
        &mut self,
        profile: &ResourceQuotaProfileV1,
        reservation: &QuotaReservationV1,
    ) -> Result<(), QuotaErrorV1> {
        let Some(active) = self
            .active_reservations
            .get(&reservation.reservation_epoch)
            .cloned()
        else {
            return Ok(());
        };
        if active.class != profile.class
            || active.reserved_bytes != reservation.reserved_bytes
            || active.reserved_entries != reservation.reserved_entries
        {
            return Ok(());
        }
        self.release_active(reservation.reservation_epoch, active)
    }

    pub fn workspace_used_bytes(&self) -> u64 {
        self.workspace_bytes
    }

    fn apply_replayed_reservation(
        &mut self,
        reservation_epoch: u64,
        owner_key: &str,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), QuotaErrorV1> {
        if reservation_epoch == 0 || self.active_reservations.contains_key(&reservation_epoch) {
            return Err(QuotaErrorV1::Journal(
                "quota journal contains an invalid reservation epoch".to_owned(),
            ));
        }
        if profile.hard_runtime_enforcement_required
            && profile.class == ResourceQuotaClassV1::BorrowedAccountingOnly
        {
            return Err(QuotaErrorV1::BorrowedClaimsEnforcement);
        }
        let class_bytes = self
            .per_class_bytes
            .get(&profile.class)
            .copied()
            .unwrap_or(0);
        let class_entries = self
            .per_class_entries
            .get(&profile.class)
            .copied()
            .unwrap_or(0);
        if class_bytes.saturating_add(bytes) > profile.max_bytes
            || class_entries.saturating_add(entries) > profile.max_entries
            || self.workspace_bytes.saturating_add(bytes) > self.workspace_cap
        {
            return Err(QuotaErrorV1::Journal(
                "quota journal replay exceeds a declared cap".to_owned(),
            ));
        }
        self.per_class_bytes
            .insert(profile.class, class_bytes.saturating_add(bytes));
        self.per_class_entries
            .insert(profile.class, class_entries.saturating_add(entries));
        self.workspace_bytes = self.workspace_bytes.saturating_add(bytes);
        self.reservation_epoch = self.reservation_epoch.max(reservation_epoch);
        self.active_reservations.insert(
            reservation_epoch,
            ActiveReservationV1 {
                owner_key: owner_key.to_owned(),
                class: profile.class,
                reserved_bytes: bytes,
                reserved_entries: entries,
            },
        );
        Ok(())
    }

    fn apply_replayed_release(
        &mut self,
        reservation_epoch: u64,
        owner_key: &str,
        class: ResourceQuotaClassV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), QuotaErrorV1> {
        let Some(active) = self.active_reservations.get(&reservation_epoch).cloned() else {
            return Err(QuotaErrorV1::Journal(
                "quota journal releases an unknown reservation".to_owned(),
            ));
        };
        if active.class != class
            || active.owner_key != owner_key
            || active.reserved_bytes != bytes
            || active.reserved_entries != entries
        {
            return Err(QuotaErrorV1::Journal(
                "quota journal release does not match reservation".to_owned(),
            ));
        }
        self.apply_release(reservation_epoch, class, bytes, entries);
        Ok(())
    }

    fn release_active(
        &mut self,
        reservation_epoch: u64,
        active: ActiveReservationV1,
    ) -> Result<(), QuotaErrorV1> {
        if let Some(journal) = self.journal.as_mut() {
            journal.append(QuotaJournalEventV1::Released {
                reservation_epoch,
                owner_key: active.owner_key.clone(),
                class: active.class,
                reserved_bytes: active.reserved_bytes,
                reserved_entries: active.reserved_entries,
            })?;
        }
        self.apply_release(
            reservation_epoch,
            active.class,
            active.reserved_bytes,
            active.reserved_entries,
        );
        Ok(())
    }

    fn apply_release(
        &mut self,
        reservation_epoch: u64,
        class: ResourceQuotaClassV1,
        bytes: u64,
        entries: u64,
    ) {
        let used_bytes = self.per_class_bytes.get(&class).copied().unwrap_or(0);
        let used_entries = self.per_class_entries.get(&class).copied().unwrap_or(0);
        self.per_class_bytes
            .insert(class, used_bytes.saturating_sub(bytes));
        self.per_class_entries
            .insert(class, used_entries.saturating_sub(entries));
        self.workspace_bytes = self.workspace_bytes.saturating_sub(bytes);
        self.active_reservations.remove(&reservation_epoch);
    }
}

pub fn quota_class_label(class: ResourceQuotaClassV1) -> &'static str {
    match class {
        ResourceQuotaClassV1::AttemptEphemeral => "attempt-ephemeral",
        ResourceQuotaClassV1::SessionScratch => "session-scratch",
        ResourceQuotaClassV1::RuntimeState => "runtime-state",
        ResourceQuotaClassV1::RuntimeCache => "runtime-cache",
        ResourceQuotaClassV1::ArtifactStaging => "artifact-staging",
        ResourceQuotaClassV1::ArtifactStore => "artifact-store",
        ResourceQuotaClassV1::IsolatedWorkspace => "isolated-workspace",
        ResourceQuotaClassV1::ToolCache => "tool-cache",
        ResourceQuotaClassV1::BorrowedAccountingOnly => "borrowed-accounting",
        ResourceQuotaClassV1::Quarantine => "quarantine",
    }
}

#[cfg(test)]
#[path = "tests/quota_tests.rs"]
mod tests;
