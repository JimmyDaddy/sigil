//! Authority-owned durable process inventory for RFC-0071 bootstrap recovery.
//!
//! Registration is deliberately a two-phase protocol: a prepared record is durably published
//! before the host may spawn, then the OS process id is attached immediately after spawn. Both a
//! prepared record and a live attached record block fresh-epoch recovery. Terminal settlement is
//! the only operation that removes an entry.

use std::{collections::BTreeMap, sync::Mutex};

use serde::{Deserialize, Serialize};
use sigil_kernel::resource::CanonicalHash;

use crate::bootstrap::{
    AuthorityBootstrapObjectClassV1, AuthorityBootstrapPublicationGuard, AuthorityBootstrapStoreV1,
    BootstrapErrorV1,
};

const PROCESS_INVENTORY_SCHEMA_VERSION: u32 = 1;
const PROCESS_INVENTORY_REQUIREMENT_SCHEMA_VERSION: u32 = 1;
const MAX_ACTIVE_PROCESS_ENTRIES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AuthorityProcessInventoryStateV1 {
    Prepared,
    Attached { process_id: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthorityProcessInventoryEntryV1 {
    pub(crate) attempt_id: String,
    pub(crate) state: AuthorityProcessInventoryStateV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthorityProcessInventorySnapshotV1 {
    pub(crate) schema_version: u32,
    pub(crate) authority_epoch: u64,
    pub(crate) sequence: u64,
    pub(crate) entries: BTreeMap<String, AuthorityProcessInventoryEntryV1>,
    pub(crate) snapshot_hash: CanonicalHash,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct AuthorityProcessInventoryRequirementV1 {
    schema_version: u32,
    authority_epoch: u64,
    marker_hash: CanonicalHash,
}

impl AuthorityProcessInventoryRequirementV1 {
    fn new(authority_epoch: u64) -> Self {
        let marker_hash = requirement_hash(authority_epoch);
        Self {
            schema_version: PROCESS_INVENTORY_REQUIREMENT_SCHEMA_VERSION,
            authority_epoch,
            marker_hash,
        }
    }

    fn validate(&self, expected_epoch: u64) -> Result<(), BootstrapErrorV1> {
        if self.schema_version != PROCESS_INVENTORY_REQUIREMENT_SCHEMA_VERSION
            || self.authority_epoch != expected_epoch
            || self.marker_hash != requirement_hash(expected_epoch)
        {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "authority process inventory requirement marker is invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

impl AuthorityProcessInventorySnapshotV1 {
    pub(crate) fn empty(authority_epoch: u64) -> Self {
        let mut snapshot = Self {
            schema_version: PROCESS_INVENTORY_SCHEMA_VERSION,
            authority_epoch,
            sequence: 0,
            entries: BTreeMap::new(),
            snapshot_hash: CanonicalHash::from_bytes([0; 32]),
        };
        snapshot.snapshot_hash = snapshot.compute_hash();
        snapshot
    }

    pub(crate) fn compute_hash(&self) -> CanonicalHash {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(&(
            self.schema_version,
            self.authority_epoch,
            self.sequence,
            &self.entries,
        ))
        .expect("bounded process inventory is serializable");
        CanonicalHash::from_bytes(Sha256::digest(bytes).into())
    }

    pub(crate) fn validate(&self, expected_epoch: u64) -> Result<(), BootstrapErrorV1> {
        if self.schema_version != PROCESS_INVENTORY_SCHEMA_VERSION
            || self.authority_epoch != expected_epoch
            || self.entries.len() > MAX_ACTIVE_PROCESS_ENTRIES
            || self.snapshot_hash != self.compute_hash()
            || self.entries.iter().any(|(key, entry)| {
                key.is_empty() || key != &entry.attempt_id || entry.attempt_id.len() > 512
            })
        {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "authority process inventory is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<(), AuthorityProcessInventoryErrorV1> {
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(AuthorityProcessInventoryErrorV1::SequenceExhausted)?;
        self.snapshot_hash = self.compute_hash();
        Ok(())
    }
}

/// Non-forgeable claim returned only after the prepared record is durable.
#[derive(Debug, PartialEq, Eq)]
pub struct AuthorityProcessInventoryClaimV1 {
    attempt_id: String,
    authority_epoch: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthorityProcessInventoryErrorV1 {
    #[error("authority process inventory bootstrap failed: {0}")]
    Bootstrap(#[from] BootstrapErrorV1),
    #[error("authority process inventory capacity exhausted")]
    CapacityExhausted,
    #[error("authority process inventory sequence exhausted")]
    SequenceExhausted,
    #[error("authority process inventory claim is stale or invalid")]
    InvalidClaim,
    #[error("authority process inventory lock is poisoned")]
    LockPoisoned,
}

/// Pathless sandbox-facing lifecycle port. Implementations must persist `prepare_spawn` before
/// the physical spawn and must not settle an attached claim until the child is reaped.
pub trait AuthorityProcessInventoryPortV1: Send + Sync {
    fn prepare_spawn(
        &self,
        attempt_id: &str,
    ) -> Result<AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1>;

    fn attach_spawn(
        &self,
        claim: &AuthorityProcessInventoryClaimV1,
        process_id: u32,
    ) -> Result<(), AuthorityProcessInventoryErrorV1>;

    fn settle_spawn(
        &self,
        claim: AuthorityProcessInventoryClaimV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1>;
}

/// Bootstrap-store-backed process inventory for the active authority epoch.
#[derive(Debug)]
pub struct AuthorityManagedProcessInventoryV1 {
    store: AuthorityBootstrapStoreV1,
    local_update: Mutex<()>,
}

impl AuthorityManagedProcessInventoryV1 {
    /// Initializes or verifies the current epoch inventory while the boot publication lock is
    /// already held. A missing inventory is created only for a freshly selected bootstrap epoch.
    pub fn initialize(
        store: AuthorityBootstrapStoreV1,
        guard: &AuthorityBootstrapPublicationGuard,
        allow_pre_inventory_cutover_seed: bool,
    ) -> Result<Self, AuthorityProcessInventoryErrorV1> {
        let marker: Option<AuthorityProcessInventoryRequirementV1> = store.read_json(
            guard,
            AuthorityBootstrapObjectClassV1::ProcessInventoryRequirement,
        )?;
        let snapshot: Option<AuthorityProcessInventorySnapshotV1> =
            store.read_json(guard, AuthorityBootstrapObjectClassV1::ProcessInventory)?;
        if let Some(marker) = &marker {
            marker.validate(store.authority_epoch())?;
        }
        match (marker, snapshot) {
            (Some(_), Some(snapshot)) => snapshot.validate(store.authority_epoch())?,
            (Some(_), None) => {
                return Err(BootstrapErrorV1::MetadataCorrupted(
                    "required authority process inventory is missing".to_owned(),
                )
                .into());
            }
            (None, Some(snapshot)) => {
                snapshot.validate(store.authority_epoch())?;
                publish_requirement(&store, guard)?;
            }
            (None, None)
                if store.was_created_for_this_open() || allow_pre_inventory_cutover_seed =>
            {
                let snapshot = AuthorityProcessInventorySnapshotV1::empty(store.authority_epoch());
                publish_snapshot(&store, guard, &snapshot)?;
                publish_requirement(&store, guard)?;
            }
            (None, None) => {
                return Err(BootstrapErrorV1::MetadataCorrupted(
                    "required authority process inventory and requirement marker are missing"
                        .to_owned(),
                )
                .into());
            }
        }
        Ok(Self {
            store,
            local_update: Mutex::new(()),
        })
    }

    fn mutate(
        &self,
        mutate: impl FnOnce(
            &mut AuthorityProcessInventorySnapshotV1,
        ) -> Result<(), AuthorityProcessInventoryErrorV1>,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        let _local = self
            .local_update
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?;
        let guard = self.store.acquire_publication()?;
        let mut snapshot: AuthorityProcessInventorySnapshotV1 = self
            .store
            .read_json(&guard, AuthorityBootstrapObjectClassV1::ProcessInventory)?
            .ok_or_else(|| {
                BootstrapErrorV1::MetadataCorrupted(
                    "authority process inventory disappeared".to_owned(),
                )
            })?;
        snapshot.validate(self.store.authority_epoch())?;
        mutate(&mut snapshot)?;
        snapshot.advance()?;
        publish_snapshot(&self.store, &guard, &snapshot)
    }
}

impl AuthorityProcessInventoryPortV1 for AuthorityManagedProcessInventoryV1 {
    fn prepare_spawn(
        &self,
        attempt_id: &str,
    ) -> Result<AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1> {
        if attempt_id.is_empty() || attempt_id.len() > 512 {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        self.mutate(|snapshot| {
            if snapshot.entries.len() >= MAX_ACTIVE_PROCESS_ENTRIES {
                return Err(AuthorityProcessInventoryErrorV1::CapacityExhausted);
            }
            if snapshot.entries.contains_key(attempt_id) {
                return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
            }
            snapshot.entries.insert(
                attempt_id.to_owned(),
                AuthorityProcessInventoryEntryV1 {
                    attempt_id: attempt_id.to_owned(),
                    state: AuthorityProcessInventoryStateV1::Prepared,
                },
            );
            Ok(())
        })?;
        Ok(AuthorityProcessInventoryClaimV1 {
            attempt_id: attempt_id.to_owned(),
            authority_epoch: self.store.authority_epoch(),
        })
    }

    fn attach_spawn(
        &self,
        claim: &AuthorityProcessInventoryClaimV1,
        process_id: u32,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        if process_id == 0 || claim.authority_epoch != self.store.authority_epoch() {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        self.mutate(|snapshot| {
            let entry = snapshot
                .entries
                .get_mut(&claim.attempt_id)
                .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
            if !matches!(entry.state, AuthorityProcessInventoryStateV1::Prepared) {
                return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
            }
            entry.state = AuthorityProcessInventoryStateV1::Attached { process_id };
            Ok(())
        })
    }

    fn settle_spawn(
        &self,
        claim: AuthorityProcessInventoryClaimV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        if claim.authority_epoch != self.store.authority_epoch() {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        self.mutate(|snapshot| {
            snapshot
                .entries
                .remove(&claim.attempt_id)
                .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
            Ok(())
        })
    }
}

fn publish_snapshot(
    store: &AuthorityBootstrapStoreV1,
    guard: &AuthorityBootstrapPublicationGuard,
    snapshot: &AuthorityProcessInventorySnapshotV1,
) -> Result<(), AuthorityProcessInventoryErrorV1> {
    let bytes = serde_json::to_vec(snapshot).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory serialization failed: {error}"
        ))
    })?;
    store.publish_bytes(
        guard,
        AuthorityBootstrapObjectClassV1::ProcessInventory,
        &bytes,
    )?;
    Ok(())
}

fn requirement_hash(authority_epoch: u64) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"authority-process-inventory-required-v1");
    hasher.update(authority_epoch.to_le_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn publish_requirement(
    store: &AuthorityBootstrapStoreV1,
    guard: &AuthorityBootstrapPublicationGuard,
) -> Result<(), AuthorityProcessInventoryErrorV1> {
    let marker = AuthorityProcessInventoryRequirementV1::new(store.authority_epoch());
    let bytes = serde_json::to_vec(&marker).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory requirement serialization failed: {error}"
        ))
    })?;
    store.publish_bytes(
        guard,
        AuthorityBootstrapObjectClassV1::ProcessInventoryRequirement,
        &bytes,
    )?;
    Ok(())
}

/// In-memory test adapter. It is feature-gated and cannot be linked by shipping builds.
#[cfg(feature = "test-support")]
#[derive(Debug, Default)]
pub struct InMemoryAuthorityProcessInventoryV1 {
    entries: Mutex<BTreeMap<String, AuthorityProcessInventoryStateV1>>,
}

#[cfg(feature = "test-support")]
impl AuthorityProcessInventoryPortV1 for InMemoryAuthorityProcessInventoryV1 {
    fn prepare_spawn(
        &self,
        attempt_id: &str,
    ) -> Result<AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?;
        if entries
            .insert(
                attempt_id.to_owned(),
                AuthorityProcessInventoryStateV1::Prepared,
            )
            .is_some()
        {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        Ok(AuthorityProcessInventoryClaimV1 {
            attempt_id: attempt_id.to_owned(),
            authority_epoch: 1,
        })
    }

    fn attach_spawn(
        &self,
        claim: &AuthorityProcessInventoryClaimV1,
        process_id: u32,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?;
        let state = entries
            .get_mut(&claim.attempt_id)
            .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
        if !matches!(state, AuthorityProcessInventoryStateV1::Prepared) {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        *state = AuthorityProcessInventoryStateV1::Attached { process_id };
        Ok(())
    }

    fn settle_spawn(
        &self,
        claim: AuthorityProcessInventoryClaimV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        self.entries
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?
            .remove(&claim.attempt_id)
            .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
        Ok(())
    }
}
