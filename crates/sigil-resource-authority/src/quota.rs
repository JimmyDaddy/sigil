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
    #[error("preferred quota capacity is below the required minimum")]
    InvalidCapacityRange,
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
    Adjusted {
        previous_reservation_epoch: u64,
        previous_class: ResourceQuotaClassV1,
        previous_bytes: u64,
        previous_entries: u64,
        reservation_epoch: u64,
        owner_key: String,
        profile: ResourceQuotaProfileV1,
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
    #[cfg(test)]
    persistence_failure: std::cell::Cell<Option<QuotaPersistenceFailurePoint>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QuotaPersistenceFailurePoint {
    FileSync,
    DirectorySync,
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
            // A policy increase cannot make an invalid predecessor journal valid. Verify every
            // historical reservation under its original ceiling before applying the new cap.
            let mut book = QuotaBookV1::new(snapshot.workspace_cap);
            verify_and_replay_records(&mut book, &snapshot.records)?;
            book.workspace_cap = workspace_cap;
            let expected_predecessor = snapshot.clone();
            let journal = Self {
                path,
                workspace_cap,
                records: snapshot.records,
                poisoned: false,
                #[cfg(test)]
                persistence_failure: std::cell::Cell::new(None),
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
                #[cfg(test)]
                persistence_failure: std::cell::Cell::new(None),
            };
            journal.persist(None)?;
            Ok((journal, QuotaBookV1::new(workspace_cap)))
        }
    }

    fn append(&mut self, event: QuotaJournalEventV1) -> Result<(), QuotaErrorV1> {
        self.ensure_healthy()?;
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

    fn ensure_healthy(&self) -> Result<(), QuotaErrorV1> {
        if self.poisoned {
            return Err(QuotaErrorV1::Journal(
                "journal is poisoned after an uncertain durability failure".to_owned(),
            ));
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
            #[cfg(test)]
            if self.persistence_failure.get() == Some(QuotaPersistenceFailurePoint::FileSync) {
                self.persistence_failure.set(None);
                return Err(quota_io_error(std::io::Error::other(
                    "injected file fsync failure",
                )));
            }
            file.sync_all().map_err(quota_io_error)?;
            fs::rename(&temp_path, &self.path).map_err(quota_io_error)?;
            #[cfg(test)]
            if self.persistence_failure.get() == Some(QuotaPersistenceFailurePoint::DirectorySync) {
                self.persistence_failure.set(None);
                return Err(quota_uncertain_io_error(std::io::Error::other(
                    "injected directory fsync failure",
                )));
            }
            #[cfg(unix)]
            File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(quota_uncertain_io_error)?;
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

#[cfg(any(unix, test))]
fn quota_uncertain_io_error(error: std::io::Error) -> QuotaErrorV1 {
    QuotaErrorV1::Journal(format!(
        "uncertain durability after quota snapshot installation: {error}"
    ))
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
            QuotaJournalEventV1::Adjusted {
                previous_reservation_epoch,
                previous_class,
                previous_bytes,
                previous_entries,
                reservation_epoch,
                owner_key,
                profile,
                reserved_bytes,
                reserved_entries,
            } => {
                let previous = book
                    .active_reservations
                    .get(previous_reservation_epoch)
                    .filter(|previous| {
                        previous.owner_key == *owner_key
                            && previous.class == *previous_class
                            && previous.reserved_bytes == *previous_bytes
                            && previous.reserved_entries == *previous_entries
                    })
                    .cloned()
                    .ok_or_else(|| {
                        QuotaErrorV1::Journal(
                            "quota adjustment does not match its predecessor".to_owned(),
                        )
                    })?;
                if *reservation_epoch <= book.reservation_epoch {
                    return Err(QuotaErrorV1::Journal(
                        "quota adjustment has a stale reservation epoch".to_owned(),
                    ));
                }
                book.validate_replacement(
                    Some(&previous),
                    profile,
                    *reserved_bytes,
                    *reserved_entries,
                )?;
                book.apply_release(
                    *previous_reservation_epoch,
                    *previous_class,
                    *previous_bytes,
                    *previous_entries,
                );
                book.apply_replayed_reservation(
                    *reservation_epoch,
                    owner_key,
                    profile,
                    *reserved_bytes,
                    *reserved_entries,
                )?;
            }
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

    /// Checks whether previously granted capacity can still authorize a physical write.
    /// This reads only the in-memory durability latch and performs no journal I/O.
    ///
    /// # Errors
    /// Rejects a journal poisoned by uncertain persistence until verified state is reopened.
    pub(crate) fn ensure_healthy(&self) -> Result<(), QuotaErrorV1> {
        self.journal
            .as_ref()
            .map_or(Ok(()), QuotaJournalV1::ensure_healthy)
    }

    #[cfg(test)]
    pub(crate) fn inject_next_directory_sync_failure(&self) {
        self.journal
            .as_ref()
            .expect("durable quota fault fixture")
            .persistence_failure
            .set(Some(QuotaPersistenceFailurePoint::DirectorySync));
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
        if let Some(journal) = &self.journal {
            journal.ensure_healthy()?;
        }
        let owner_key = owner_key.into();
        if !owner_key.is_empty()
            && self
                .active_reservations
                .values()
                .any(|reservation| reservation.owner_key == owner_key)
        {
            return Err(QuotaErrorV1::OwnerAlreadyReserved);
        }
        self.validate_replacement(None, profile, bytes, entries)?;
        let used_bytes = *self.per_class_bytes.get(&profile.class).unwrap_or(&0);
        let used_entries = *self.per_class_entries.get(&profile.class).unwrap_or(&0);
        let reservation_epoch = self
            .reservation_epoch
            .checked_add(1)
            .ok_or_else(|| QuotaErrorV1::Journal("quota reservation epoch exhausted".to_owned()))?;
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

    /// Atomically replaces one owner's measured usage. Admission and journal failure leave
    /// the previous reservation intact; one durable adjustment has no unreserved midpoint.
    /// The namespace authority must validate the owner's live lease and generation before
    /// calling this bookkeeping operation and serialize it with the corresponding mutation.
    ///
    /// # Errors
    /// Fails if the profile claims unsupported enforcement, a byte/entry/workspace limit would
    /// be exceeded, the reservation epoch is exhausted, or journal CAS/persistence fails. The
    /// old in-memory charge is retained on rejection; uncertain durability poisons the journal
    /// and requires reopening verified durable state before further mutation.
    pub fn reconcile_owned(
        &mut self,
        owner_key: impl Into<String>,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<QuotaReservationV1, QuotaErrorV1> {
        if let Some(journal) = &self.journal {
            journal.ensure_healthy()?;
        }
        let owner_key = owner_key.into();
        if let Some((epoch, active)) = self
            .active_reservations
            .iter()
            .find(|(_, reservation)| reservation.owner_key == owner_key)
            .map(|(epoch, reservation)| (*epoch, reservation.clone()))
        {
            self.validate_replacement(Some(&active), profile, bytes, entries)?;
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
            let reservation_epoch = self.reservation_epoch.checked_add(1).ok_or_else(|| {
                QuotaErrorV1::Journal("quota reservation epoch exhausted".to_owned())
            })?;
            if let Some(journal) = self.journal.as_mut() {
                journal.append(QuotaJournalEventV1::Adjusted {
                    previous_reservation_epoch: epoch,
                    previous_class: active.class,
                    previous_bytes: active.reserved_bytes,
                    previous_entries: active.reserved_entries,
                    reservation_epoch,
                    owner_key: owner_key.clone(),
                    profile: profile.clone(),
                    reserved_bytes: bytes,
                    reserved_entries: entries,
                })?;
            }
            self.apply_release(
                epoch,
                active.class,
                active.reserved_bytes,
                active.reserved_entries,
            );
            self.apply_replayed_reservation(
                reservation_epoch,
                &owner_key,
                profile,
                bytes,
                entries,
            )?;
            return Ok(QuotaReservationV1 {
                reservation_epoch,
                reserved_bytes: bytes,
                reserved_entries: entries,
            });
        }
        self.reserve_owned(owner_key, profile, bytes, entries)
    }

    /// Reserves total byte capacity for one stable owner using currently available quota.
    /// Requires `minimum_bytes <= preferred_bytes`; the granted total lies within that range
    /// and `entries` is the exact total entry charge. Existing capacity for this owner is
    /// replaced atomically while other owners remain charged. The namespace authority must
    /// validate the same live lease and generation and serialize admission with its write;
    /// this book neither authenticates handles nor grants physical storage access.
    ///
    /// # Errors
    /// Returns [`QuotaErrorV1::InvalidCapacityRange`] for an inverted range, or a quota error
    /// when the minimum bytes or exact entries cannot fit the class/workspace limits. Also
    /// rejects unsupported enforcement, exhausted epochs, durable snapshot CAS mismatches,
    /// persistence failures and a poisoned journal. Rejection does not release the preceding
    /// charge; uncertain durability requires reopening verified state before further mutation.
    pub fn reserve_owned_capacity(
        &mut self,
        owner_key: impl Into<String>,
        profile: &ResourceQuotaProfileV1,
        minimum_bytes: u64,
        preferred_bytes: u64,
        entries: u64,
    ) -> Result<QuotaReservationV1, QuotaErrorV1> {
        if preferred_bytes < minimum_bytes {
            return Err(QuotaErrorV1::InvalidCapacityRange);
        }
        let owner_key = owner_key.into();
        let previous = self
            .active_reservations
            .values()
            .find(|active| active.owner_key == owner_key);
        self.validate_replacement(previous, profile, minimum_bytes, entries)?;
        let previous_class_bytes = previous
            .filter(|active| active.class == profile.class)
            .map_or(0, |active| active.reserved_bytes);
        let class_used = self
            .per_class_bytes
            .get(&profile.class)
            .copied()
            .unwrap_or(0)
            .saturating_sub(previous_class_bytes);
        let workspace_used = self
            .workspace_bytes
            .saturating_sub(previous.map_or(0, |active| active.reserved_bytes));
        let capacity = preferred_bytes
            .min(profile.max_bytes.saturating_sub(class_used))
            .min(self.workspace_cap.saturating_sub(workspace_used));
        self.reconcile_owned(owner_key, profile, capacity, entries)
    }

    fn validate_replacement(
        &self,
        previous: Option<&ActiveReservationV1>,
        profile: &ResourceQuotaProfileV1,
        bytes: u64,
        entries: u64,
    ) -> Result<(), QuotaErrorV1> {
        if profile.hard_runtime_enforcement_required
            && profile.class == ResourceQuotaClassV1::BorrowedAccountingOnly
        {
            return Err(QuotaErrorV1::BorrowedClaimsEnforcement);
        }
        let same_class = previous.filter(|active| active.class == profile.class);
        let class_bytes = self
            .per_class_bytes
            .get(&profile.class)
            .copied()
            .unwrap_or(0)
            .saturating_sub(same_class.map_or(0, |active| active.reserved_bytes));
        let class_entries = self
            .per_class_entries
            .get(&profile.class)
            .copied()
            .unwrap_or(0)
            .saturating_sub(same_class.map_or(0, |active| active.reserved_entries));
        let workspace_bytes = self
            .workspace_bytes
            .saturating_sub(previous.map_or(0, |active| active.reserved_bytes));
        if class_bytes > profile.max_bytes || bytes > profile.max_bytes.saturating_sub(class_bytes)
        {
            return Err(QuotaErrorV1::ReservationExceeded {
                class: quota_class_label(profile.class),
                reserved: class_bytes.saturating_add(bytes),
                max: profile.max_bytes,
            });
        }
        if class_entries > profile.max_entries
            || entries > profile.max_entries.saturating_sub(class_entries)
        {
            return Err(QuotaErrorV1::EntryExceeded {
                class: quota_class_label(profile.class),
                reserved: class_entries.saturating_add(entries),
                max: profile.max_entries,
            });
        }
        if workspace_bytes > self.workspace_cap
            || bytes > self.workspace_cap.saturating_sub(workspace_bytes)
        {
            return Err(QuotaErrorV1::WorkspaceOvercommit {
                used: workspace_bytes,
                incoming: bytes,
                cap: self.workspace_cap,
            });
        }
        Ok(())
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

    /// Releases reservations left by a previous owner process after the caller has acquired
    /// that process's exclusive authority lock. Reservations are process leases, not durable
    /// claims on a namespace; the next process re-admits current physical objects as needed.
    pub(crate) fn release_all_active(&mut self) -> Result<(), QuotaErrorV1> {
        let owners: Vec<String> = self.active_owner_keys().into_iter().collect();
        for owner in owners {
            self.release_owner(&owner)?;
        }
        Ok(())
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

    pub(crate) const fn workspace_cap(&self) -> u64 {
        self.workspace_cap
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
