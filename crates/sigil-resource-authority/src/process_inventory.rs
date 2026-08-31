//! Authority-owned durable process inventory for RFC-0071 bootstrap recovery.
//!
//! Registration is deliberately a two-phase protocol: a prepared record is durably published
//! before the host may spawn, then the sandbox submits one-shot observer evidence for the actual
//! child. The inventory persists the verifier-authenticated birth/scope subject, never a PID.
//! Prepared records and attached subjects remain recovery blockers until a higher-level durable
//! settlement path resolves them; this compact slice does not claim full-tree quiescence.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use hmac::{Hmac, Mac};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use sigil_kernel::{
    process_observation::{
        HostProcessIdentityRecoveryProbeV1, HostProcessIdentityRegistrationV1,
        HostProcessObservationFactoryV1, HostProcessObservationServiceV1,
        HostProcessObservationVerifierV1, ProcessObservationErrorV1, ProcessObservationScopeV1,
        ProcessObservationSubjectKindV1, ProcessVitalityV1, VerifiedHostProcessIdentityV1,
    },
    resource::{CanonicalHash, PhysicalAttemptId},
};

use crate::bootstrap::{
    AuthorityBootstrapObjectClassV1, AuthorityBootstrapPublicationGuard, AuthorityBootstrapStoreV1,
    BootstrapErrorV1,
};

const PROCESS_INVENTORY_SCHEMA_VERSION: u32 = 2;
const PROCESS_INVENTORY_REQUIREMENT_SCHEMA_VERSION: u32 = 2;
const PROCESS_INVENTORY_AUTHENTICATOR_SCHEMA_VERSION: u32 = 1;
const MAX_ACTIVE_PROCESS_ENTRIES: usize = 256;
const INVENTORY_RECORD_AUTH_DOMAIN: &[u8] = b"sigil-authority-process-inventory-record-v2\0";
const BOUNDED_NATIVE_EXPOSURE_DOMAIN: &[u8] =
    b"sigil-authority-process-inventory-bounded-native-v2\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum AuthorityProcessInventoryAuthenticatorStateV1 {
    PendingInventory,
    Active,
}

/// Bootstrap-owned durable record key and realm.
///
/// This material remains in the fixed, no-follow owner-only bootstrap object. It authenticates
/// persisted inventory facts across observer restarts; it is not an observer live-signing key and
/// never leaves Resource Authority.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthorityProcessInventoryAuthenticatorV1 {
    schema_version: u32,
    state: AuthorityProcessInventoryAuthenticatorStateV1,
    realm_id: String,
    key_id: String,
    key_material: Vec<u8>,
}

impl fmt::Debug for AuthorityProcessInventoryAuthenticatorV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityProcessInventoryAuthenticatorV1")
            .field("schema_version", &self.schema_version)
            .field("state", &self.state)
            .field("realm_id", &self.realm_id)
            .field("key_id", &self.key_id)
            .field("key_material", &"[redacted]")
            .finish()
    }
}

impl AuthorityProcessInventoryAuthenticatorV1 {
    fn create() -> Result<Self, AuthorityProcessInventoryErrorV1> {
        let mut material = [0_u8; 64];
        SystemRandom::new()
            .fill(&mut material)
            .map_err(|_| AuthorityProcessInventoryErrorV1::AuthenticatorUnavailable)?;
        let key_material = material[..32].to_vec();
        let realm_id = CanonicalHash::from_bytes(
            material[32..]
                .try_into()
                .map_err(|_| AuthorityProcessInventoryErrorV1::AuthenticatorUnavailable)?,
        )
        .to_hex();
        let key_id = key_id(&key_material)?;
        Ok(Self {
            schema_version: PROCESS_INVENTORY_AUTHENTICATOR_SCHEMA_VERSION,
            state: AuthorityProcessInventoryAuthenticatorStateV1::PendingInventory,
            realm_id,
            key_id,
            key_material,
        })
    }

    fn validate(&self) -> Result<(), AuthorityProcessInventoryErrorV1> {
        if self.schema_version != PROCESS_INVENTORY_AUTHENTICATOR_SCHEMA_VERSION
            || self.key_material.len() != 32
            || self.realm_id.len() != 64
            || self.key_id != key_id(&self.key_material)?
        {
            return Err(AuthorityProcessInventoryErrorV1::AuthenticatorCorrupted);
        }
        Ok(())
    }

    fn activate(&mut self) {
        self.state = AuthorityProcessInventoryAuthenticatorStateV1::Active;
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state == AuthorityProcessInventoryAuthenticatorStateV1::Active
    }

    fn authenticate(
        &self,
        snapshot_hash: CanonicalHash,
        previous_record_hash: Option<CanonicalHash>,
    ) -> Result<CanonicalHash, AuthorityProcessInventoryErrorV1> {
        let mac = self.record_mac(snapshot_hash, previous_record_hash)?;
        Ok(CanonicalHash::from_bytes(
            mac.finalize().into_bytes().into(),
        ))
    }

    fn verify_record_authenticator(
        &self,
        snapshot_hash: CanonicalHash,
        previous_record_hash: Option<CanonicalHash>,
        record_authenticator: CanonicalHash,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        self.record_mac(snapshot_hash, previous_record_hash)?
            .verify_slice(record_authenticator.as_bytes())
            .map_err(|_| AuthorityProcessInventoryErrorV1::AuthenticatorCorrupted)
    }

    fn record_mac(
        &self,
        snapshot_hash: CanonicalHash,
        previous_record_hash: Option<CanonicalHash>,
    ) -> Result<Hmac<Sha256>, AuthorityProcessInventoryErrorV1> {
        self.validate()?;
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key_material)
            .map_err(|_| AuthorityProcessInventoryErrorV1::AuthenticatorUnavailable)?;
        mac.update(INVENTORY_RECORD_AUTH_DOMAIN);
        update_auth_field(&mut mac, self.realm_id.as_bytes());
        update_auth_field(&mut mac, self.key_id.as_bytes());
        mac.update(snapshot_hash.as_bytes());
        match previous_record_hash {
            Some(previous) => {
                mac.update(&[1]);
                mac.update(previous.as_bytes());
            }
            None => mac.update(&[0]),
        }
        Ok(mac)
    }
}

fn key_id(key_material: &[u8]) -> Result<String, AuthorityProcessInventoryErrorV1> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key_material)
        .map_err(|_| AuthorityProcessInventoryErrorV1::AuthenticatorUnavailable)?;
    mac.update(b"sigil-authority-process-inventory-key-id-v1\0");
    Ok(CanonicalHash::from_bytes(mac.finalize().into_bytes().into()).to_hex())
}

fn update_auth_field(mac: &mut Hmac<Sha256>, value: &[u8]) {
    mac.update(&(value.len() as u64).to_be_bytes());
    mac.update(value);
}

/// Composition context that the authority binds to its self-owner registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityProcessInventoryBootstrapBindingV1 {
    pub application_composition_epoch: u64,
    pub owner_execution_scope_hash: CanonicalHash,
}

impl AuthorityProcessInventoryBootstrapBindingV1 {
    fn owner_scope(
        &self,
        authority_epoch: u64,
    ) -> Result<ProcessObservationScopeV1, AuthorityProcessInventoryErrorV1> {
        let scope = ProcessObservationScopeV1 {
            authority_epoch,
            application_composition_epoch: self.application_composition_epoch,
            execution_scope_hash: self.owner_execution_scope_hash,
        };
        if !scope.is_well_formed() {
            return Err(AuthorityProcessInventoryErrorV1::InvalidBootstrapBinding);
        }
        Ok(scope)
    }
}

/// Prepared physical attempt data bound before its child can be registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorityProcessSpawnRequestV1 {
    pub attempt_id: PhysicalAttemptId,
    pub execution_scope_hash: CanonicalHash,
}

impl AuthorityProcessSpawnRequestV1 {
    fn scope(
        &self,
        authority_epoch: u64,
        application_composition_epoch: u64,
    ) -> Result<ProcessObservationScopeV1, AuthorityProcessInventoryErrorV1> {
        if self.attempt_id.as_str().is_empty() || self.attempt_id.as_str().len() > 512 {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        let scope = ProcessObservationScopeV1 {
            authority_epoch,
            application_composition_epoch,
            execution_scope_hash: self.execution_scope_hash,
        };
        if !scope.is_well_formed() {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        Ok(scope)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AuthorityProcessInventoryStateV1 {
    Prepared {
        scope: ProcessObservationScopeV1,
    },
    Attached {
        subject: VerifiedHostProcessIdentityV1,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthorityProcessInventoryEntryV1 {
    pub(crate) attempt_id: PhysicalAttemptId,
    pub(crate) state: AuthorityProcessInventoryStateV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthorityProcessInventorySnapshotV1 {
    pub(crate) schema_version: u32,
    pub(crate) authority_epoch: u64,
    pub(crate) authentication_realm_id: String,
    pub(crate) authentication_key_id: String,
    pub(crate) previous_record_hash: Option<CanonicalHash>,
    pub(crate) owner_subject: VerifiedHostProcessIdentityV1,
    pub(crate) sequence: u64,
    /// Never-cleared epoch summary for native attempts that lack strong tree coverage.
    ///
    /// This is recorded with the durable pre-spawn claim. A launcher can create a child and then
    /// fail before its real birth registration reaches this inventory; settling that claim must
    /// never make a later fresh-epoch verifier mistake the empty active set for full coverage.
    pub(crate) bounded_native_exposure_count: u64,
    pub(crate) bounded_native_exposure_frontier: Option<CanonicalHash>,
    pub(crate) entries: BTreeMap<String, AuthorityProcessInventoryEntryV1>,
    pub(crate) snapshot_hash: CanonicalHash,
    pub(crate) record_authenticator: CanonicalHash,
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

    fn validate(&self, expected_epoch: u64) -> Result<(), AuthorityProcessInventoryErrorV1> {
        if self.schema_version != PROCESS_INVENTORY_REQUIREMENT_SCHEMA_VERSION {
            return Err(AuthorityProcessInventoryErrorV1::LegacySchema);
        }
        if self.authority_epoch != expected_epoch
            || self.marker_hash != requirement_hash(expected_epoch)
        {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "authority process inventory requirement marker is invalid".to_owned(),
            )
            .into());
        }
        Ok(())
    }
}

impl AuthorityProcessInventorySnapshotV1 {
    pub(crate) fn new(
        authority_epoch: u64,
        owner_subject: VerifiedHostProcessIdentityV1,
        authenticator: &AuthorityProcessInventoryAuthenticatorV1,
    ) -> Result<Self, AuthorityProcessInventoryErrorV1> {
        let mut snapshot = Self {
            schema_version: PROCESS_INVENTORY_SCHEMA_VERSION,
            authority_epoch,
            authentication_realm_id: authenticator.realm_id.clone(),
            authentication_key_id: authenticator.key_id.clone(),
            previous_record_hash: None,
            owner_subject,
            sequence: 0,
            bounded_native_exposure_count: 0,
            bounded_native_exposure_frontier: None,
            entries: BTreeMap::new(),
            snapshot_hash: CanonicalHash::from_bytes([0; 32]),
            record_authenticator: CanonicalHash::from_bytes([0; 32]),
        };
        snapshot.snapshot_hash = snapshot.compute_hash();
        snapshot.record_authenticator = authenticator.authenticate(snapshot.snapshot_hash, None)?;
        Ok(snapshot)
    }

    pub(crate) fn compute_hash(&self) -> CanonicalHash {
        use sha2::{Digest, Sha256};
        let bytes = serde_json::to_vec(&(
            self.schema_version,
            self.authority_epoch,
            &self.authentication_realm_id,
            &self.authentication_key_id,
            self.previous_record_hash,
            &self.owner_subject,
            self.sequence,
            self.bounded_native_exposure_count,
            self.bounded_native_exposure_frontier,
            &self.entries,
        ))
        .expect("bounded process inventory is serializable");
        CanonicalHash::from_bytes(Sha256::digest(bytes).into())
    }

    pub(crate) fn validate(
        &self,
        expected_epoch: u64,
        authenticator: &AuthorityProcessInventoryAuthenticatorV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        if self.schema_version != PROCESS_INVENTORY_SCHEMA_VERSION {
            return Err(AuthorityProcessInventoryErrorV1::LegacySchema);
        }
        authenticator.validate()?;
        let owner_scope = self.owner_subject.scope();
        let owner_is_valid = self.owner_subject.process_id() != 0
            && self.owner_subject.has_exact_binding(
                ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
                owner_scope,
            )
            && owner_scope.authority_epoch == expected_epoch
            && owner_scope.is_well_formed();
        let entries_are_valid = self.entries.iter().all(|(key, entry)| {
            key == entry.attempt_id.as_str()
                && !key.is_empty()
                && key.len() <= 512
                && match &entry.state {
                    AuthorityProcessInventoryStateV1::Prepared { scope } => {
                        scope.authority_epoch == expected_epoch && scope.is_well_formed()
                    }
                    AuthorityProcessInventoryStateV1::Attached { subject } => {
                        subject.process_id() != 0
                            && subject.has_exact_binding(
                                ProcessObservationSubjectKindV1::ManagedExecution,
                                subject.scope(),
                            )
                            && subject.scope().authority_epoch == expected_epoch
                            && subject.scope().is_well_formed()
                    }
                }
        });
        if self.authority_epoch != expected_epoch
            || self.entries.len() > MAX_ACTIVE_PROCESS_ENTRIES
            || (self.bounded_native_exposure_count == 0
                && self.bounded_native_exposure_frontier.is_some())
            || (self.bounded_native_exposure_count != 0
                && self.bounded_native_exposure_frontier.is_none())
            || !owner_is_valid
            || !entries_are_valid
            || self.authentication_realm_id != authenticator.realm_id
            || self.authentication_key_id != authenticator.key_id
            || self.snapshot_hash != self.compute_hash()
        {
            return Err(BootstrapErrorV1::MetadataCorrupted(
                "authority process inventory is invalid".to_owned(),
            )
            .into());
        }
        authenticator.verify_record_authenticator(
            self.snapshot_hash,
            self.previous_record_hash,
            self.record_authenticator,
        )?;
        Ok(())
    }

    fn advance(
        &mut self,
        authenticator: &AuthorityProcessInventoryAuthenticatorV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        self.previous_record_hash = Some(self.record_authenticator);
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or(AuthorityProcessInventoryErrorV1::SequenceExhausted)?;
        self.snapshot_hash = self.compute_hash();
        self.record_authenticator =
            authenticator.authenticate(self.snapshot_hash, self.previous_record_hash)?;
        Ok(())
    }

    fn record_bounded_native_exposure(
        &mut self,
        attempt_id: &PhysicalAttemptId,
        scope: &ProcessObservationScopeV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        use sha2::{Digest, Sha256};

        self.bounded_native_exposure_count = self
            .bounded_native_exposure_count
            .checked_add(1)
            .ok_or(AuthorityProcessInventoryErrorV1::SequenceExhausted)?;
        let mut hasher = Sha256::new();
        hasher.update(BOUNDED_NATIVE_EXPOSURE_DOMAIN);
        match self.bounded_native_exposure_frontier {
            Some(frontier) => {
                hasher.update([1]);
                hasher.update(frontier.as_bytes());
            }
            None => hasher.update([0]),
        }
        hasher.update(attempt_id.as_str().as_bytes());
        hasher.update(scope.authority_epoch.to_be_bytes());
        hasher.update(scope.application_composition_epoch.to_be_bytes());
        hasher.update(scope.execution_scope_hash.as_bytes());
        hasher.update(self.bounded_native_exposure_count.to_be_bytes());
        self.bounded_native_exposure_frontier =
            Some(CanonicalHash::from_bytes(hasher.finalize().into()));
        Ok(())
    }
}

/// Non-cloneable claim returned only after the prepared record is durable.
///
/// The sandbox may ask this claim to observe the process it just spawned, but it cannot attach a
/// bare PID to the inventory. The returned registration is one-shot and is verified by the
/// inventory's same-factory verifier before persistence.
pub struct AuthorityProcessInventoryClaimV1 {
    attempt_id: PhysicalAttemptId,
    scope: ProcessObservationScopeV1,
    registration_service: Arc<dyn HostProcessObservationServiceV1>,
}

impl fmt::Debug for AuthorityProcessInventoryClaimV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityProcessInventoryClaimV1")
            .field("attempt_id", &self.attempt_id)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl AuthorityProcessInventoryClaimV1 {
    /// Observes the concrete child held by the sandbox immediately after spawn.
    pub fn register_spawned_process(
        &self,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, AuthorityProcessInventoryErrorV1> {
        self.registration_service
            .register_spawned_process(self.scope.clone(), process_id)
            .map_err(AuthorityProcessInventoryErrorV1::Observation)
    }

    #[must_use]
    pub fn attempt_id(&self) -> &PhysicalAttemptId {
        &self.attempt_id
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthorityProcessInventoryErrorV1 {
    #[error("authority process inventory bootstrap failed: {0}")]
    Bootstrap(#[from] BootstrapErrorV1),
    #[error(
        "authority process inventory uses a legacy PID-only schema; operator recovery is required"
    )]
    LegacySchema,
    #[error("authority process inventory bootstrap binding is invalid")]
    InvalidBootstrapBinding,
    #[error("authority process inventory durable authenticator is unavailable")]
    AuthenticatorUnavailable,
    #[error("authority process inventory durable authenticator is missing for existing state")]
    AuthenticatorMissing,
    #[error("authority process inventory durable authenticator is corrupted or mismatched")]
    AuthenticatorCorrupted,
    #[error("authority process inventory capacity exhausted")]
    CapacityExhausted,
    #[error("authority process inventory sequence exhausted")]
    SequenceExhausted,
    #[error("authority process inventory claim is stale or invalid")]
    InvalidClaim,
    #[error("prior authority owner remains live")]
    PriorOwnerStillLive,
    #[error("prior managed process remains live: {0}")]
    PriorManagedProcessStillLive(CanonicalHash),
    #[error("prepared process entry requires durable recovery before restart")]
    PreparedProcessRecoveryRequired,
    #[error("host process observation failed: {0}")]
    Observation(#[source] ProcessObservationErrorV1),
    #[error("authority process inventory lock is poisoned")]
    LockPoisoned,
}

/// Pathless sandbox-facing lifecycle port. Implementations must persist `prepare_spawn` before
/// physical spawn and must not settle an attached claim until the child is reaped.
pub trait AuthorityProcessInventoryPortV1: Send + Sync {
    fn prepare_spawn(
        &self,
        request: AuthorityProcessSpawnRequestV1,
    ) -> Result<AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1>;

    fn attach_spawn(
        &self,
        claim: &AuthorityProcessInventoryClaimV1,
        registration: HostProcessIdentityRegistrationV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1>;

    fn settle_spawn(
        &self,
        claim: AuthorityProcessInventoryClaimV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1>;
}

/// Bootstrap-store-backed process inventory for the active authority epoch.
pub struct AuthorityManagedProcessInventoryV1 {
    store: AuthorityBootstrapStoreV1,
    application_composition_epoch: u64,
    registration_service: Arc<dyn HostProcessObservationServiceV1>,
    verifier: Arc<dyn HostProcessObservationVerifierV1>,
    authenticator: AuthorityProcessInventoryAuthenticatorV1,
    local_update: Mutex<()>,
}

impl fmt::Debug for AuthorityManagedProcessInventoryV1 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorityManagedProcessInventoryV1")
            .field("authority_epoch", &self.store.authority_epoch())
            .field(
                "application_composition_epoch",
                &self.application_composition_epoch,
            )
            .finish_non_exhaustive()
    }
}

impl AuthorityManagedProcessInventoryV1 {
    /// Initializes or verifies the current epoch inventory while the boot publication lock is
    /// already held. Existing inventory is re-observed under the current factory before its owner
    /// can be replaced; no PID-only object is accepted or upgraded.
    pub fn initialize(
        store: AuthorityBootstrapStoreV1,
        guard: &AuthorityBootstrapPublicationGuard,
        binding: AuthorityProcessInventoryBootstrapBindingV1,
        process_factory: Arc<dyn HostProcessObservationFactoryV1>,
    ) -> Result<Self, AuthorityProcessInventoryErrorV1> {
        let owner_scope = binding.owner_scope(store.authority_epoch())?;
        let registration_service = process_factory.observation_service();
        let recovery_probe = process_factory.authority_recovery_probe();
        let verifier = process_factory.observation_verifier();
        let marker = read_requirement(&store, guard)?;
        let had_marker = marker.is_some();
        let snapshot = read_snapshot(&store, guard)?;
        let had_snapshot = snapshot.is_some();
        if let Some(marker) = &marker {
            marker.validate(store.authority_epoch())?;
        }
        let mut authenticator = match read_authenticator(&store, guard)? {
            Some(authenticator) => {
                authenticator.validate()?;
                match authenticator.state {
                    AuthorityProcessInventoryAuthenticatorStateV1::Active
                        if marker.is_none() || snapshot.is_none() =>
                    {
                        return Err(AuthorityProcessInventoryErrorV1::AuthenticatorCorrupted);
                    }
                    AuthorityProcessInventoryAuthenticatorStateV1::PendingInventory
                        if marker.is_some() && snapshot.is_none() =>
                    {
                        return Err(AuthorityProcessInventoryErrorV1::AuthenticatorCorrupted);
                    }
                    _ => {}
                }
                authenticator
            }
            None if marker.is_some() || snapshot.is_some() => {
                return Err(AuthorityProcessInventoryErrorV1::AuthenticatorMissing);
            }
            None if store.was_created_for_this_open() => {
                let authenticator = AuthorityProcessInventoryAuthenticatorV1::create()?;
                // This is a logical create-new under the single bootstrap publication lock. It
                // is deliberately published before any record that names the realm/key id.
                publish_authenticator(&store, guard, &authenticator)?;
                authenticator
            }
            None => {
                return Err(AuthorityProcessInventoryErrorV1::AuthenticatorMissing);
            }
        };
        let mut snapshot = match (marker, snapshot) {
            (Some(_), Some(snapshot)) => snapshot,
            (Some(_), None) => {
                return Err(BootstrapErrorV1::MetadataCorrupted(
                    "required authority process inventory is missing".to_owned(),
                )
                .into());
            }
            (None, Some(snapshot)) => snapshot,
            (None, None)
                if authenticator.state
                    == AuthorityProcessInventoryAuthenticatorStateV1::PendingInventory =>
            {
                let owner_subject = verify_current_owner(
                    Arc::clone(&registration_service),
                    Arc::clone(&verifier),
                    owner_scope.clone(),
                )?;
                let snapshot = AuthorityProcessInventorySnapshotV1::new(
                    store.authority_epoch(),
                    owner_subject,
                    &authenticator,
                )?;
                publish_snapshot(&store, guard, &snapshot)?;
                publish_requirement(&store, guard)?;
                snapshot
            }
            (None, None) => {
                return Err(BootstrapErrorV1::MetadataCorrupted(
                    "required authority process inventory and requirement marker are missing"
                        .to_owned(),
                )
                .into());
            }
        };
        snapshot.validate(store.authority_epoch(), &authenticator)?;
        if !had_snapshot {
            // The fresh snapshot and requirement have both been written under this publication
            // lock. Only then may the initial durable key/realm become active.
            authenticator.activate();
            publish_authenticator(&store, guard, &authenticator)?;
        } else if authenticator.state
            == AuthorityProcessInventoryAuthenticatorStateV1::PendingInventory
        {
            // Crash recovery after snapshot write is safe only because the existing snapshot was
            // just decoded and HMAC-verified with this pending key; no old material is re-signed.
            if !had_marker {
                publish_requirement(&store, guard)?;
            }
            authenticator.activate();
            publish_authenticator(&store, guard, &authenticator)?;
        }

        // A reopened same-realm inventory may proceed only after the old controller and every
        // attached child have been re-observed through the current factory. The narrow exception
        // is an in-process recomposition: a fresh registration establishes that the old owner is
        // this exact OS process birth, so its still-live managed children remain under that owner
        // instead of becoming a competing controller. Prepared claims never take that exception.
        // Quiescent children and the native-exposure history remain durable inputs to the
        // higher-level recovery/settlement protocol; absence is never a full-tree proof here.
        if had_snapshot {
            let current_owner = verify_current_owner(
                Arc::clone(&registration_service),
                Arc::clone(&verifier),
                owner_scope,
            )?;
            ensure_restart_is_safe(
                Arc::clone(&recovery_probe),
                Arc::clone(&verifier),
                &snapshot,
                &current_owner,
            )?;
            snapshot.owner_subject = current_owner;
            snapshot.advance(&authenticator)?;
            publish_snapshot(&store, guard, &snapshot)?;
        }
        Ok(Self {
            store,
            application_composition_epoch: binding.application_composition_epoch,
            registration_service,
            verifier,
            authenticator,
            local_update: Mutex::new(()),
        })
    }

    fn read_current_snapshot(
        &self,
        guard: &AuthorityBootstrapPublicationGuard,
    ) -> Result<AuthorityProcessInventorySnapshotV1, AuthorityProcessInventoryErrorV1> {
        read_snapshot(&self.store, guard)?.ok_or_else(|| {
            BootstrapErrorV1::MetadataCorrupted(
                "authority process inventory disappeared".to_owned(),
            )
            .into()
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
        let mut snapshot = self.read_current_snapshot(&guard)?;
        snapshot.validate(self.store.authority_epoch(), &self.authenticator)?;
        mutate(&mut snapshot)?;
        snapshot.advance(&self.authenticator)?;
        publish_snapshot(&self.store, &guard, &snapshot)
    }
}

impl AuthorityProcessInventoryPortV1 for AuthorityManagedProcessInventoryV1 {
    fn prepare_spawn(
        &self,
        request: AuthorityProcessSpawnRequestV1,
    ) -> Result<AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1> {
        let scope = request.scope(
            self.store.authority_epoch(),
            self.application_composition_epoch,
        )?;
        let attempt_id = request.attempt_id;
        self.mutate(|snapshot| {
            // A composition that has been superseded may finish only the exact claims it already
            // holds. Its old inventory handle cannot create another prepared attempt after the
            // durable owner has moved to a new composition epoch.
            if snapshot.owner_subject.scope().application_composition_epoch
                != self.application_composition_epoch
            {
                return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
            }
            if snapshot.entries.len() >= MAX_ACTIVE_PROCESS_ENTRIES {
                return Err(AuthorityProcessInventoryErrorV1::CapacityExhausted);
            }
            if snapshot.entries.contains_key(attempt_id.as_str()) {
                return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
            }
            snapshot.entries.insert(
                attempt_id.as_str().to_owned(),
                AuthorityProcessInventoryEntryV1 {
                    attempt_id: attempt_id.clone(),
                    state: AuthorityProcessInventoryStateV1::Prepared {
                        scope: scope.clone(),
                    },
                },
            );
            // Persist the weak-coverage frontier before physical spawn. If launch succeeds but
            // registration/attach fails, a later kill, wait, and settle must not erase that
            // possible native descendant boundary into a false fresh-epoch conclusion.
            snapshot.record_bounded_native_exposure(&attempt_id, &scope)?;
            Ok(())
        })?;
        Ok(AuthorityProcessInventoryClaimV1 {
            attempt_id,
            scope,
            registration_service: Arc::clone(&self.registration_service),
        })
    }

    fn attach_spawn(
        &self,
        claim: &AuthorityProcessInventoryClaimV1,
        registration: HostProcessIdentityRegistrationV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        let _local = self
            .local_update
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?;
        let guard = self.store.acquire_publication()?;
        let mut snapshot = self.read_current_snapshot(&guard)?;
        snapshot.validate(self.store.authority_epoch(), &self.authenticator)?;
        let entry = snapshot
            .entries
            .get(claim.attempt_id.as_str())
            .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
        if !matches!(&entry.state, AuthorityProcessInventoryStateV1::Prepared { scope } if scope == &claim.scope)
        {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        let subject = self
            .verifier
            .verify_registration(
                registration,
                ProcessObservationSubjectKindV1::ManagedExecution,
                &claim.scope,
            )
            .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
        if !subject.has_exact_binding(
            ProcessObservationSubjectKindV1::ManagedExecution,
            &claim.scope,
        ) {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        let entry = snapshot
            .entries
            .get_mut(claim.attempt_id.as_str())
            .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
        entry.state = AuthorityProcessInventoryStateV1::Attached { subject };
        snapshot.advance(&self.authenticator)?;
        publish_snapshot(&self.store, &guard, &snapshot)
    }

    fn settle_spawn(
        &self,
        claim: AuthorityProcessInventoryClaimV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        self.mutate(|snapshot| {
            let entry = snapshot
                .entries
                .get(claim.attempt_id.as_str())
                .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
            let entry_scope = match &entry.state {
                AuthorityProcessInventoryStateV1::Prepared { scope } => scope,
                AuthorityProcessInventoryStateV1::Attached { subject } => subject.scope(),
            };
            if entry_scope != &claim.scope {
                return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
            }
            snapshot.entries.remove(claim.attempt_id.as_str());
            Ok(())
        })
    }
}

fn verify_current_owner(
    registration_service: Arc<dyn HostProcessObservationServiceV1>,
    verifier: Arc<dyn HostProcessObservationVerifierV1>,
    scope: ProcessObservationScopeV1,
) -> Result<VerifiedHostProcessIdentityV1, AuthorityProcessInventoryErrorV1> {
    let registration = registration_service
        .register_current_authority_owner(scope.clone())
        .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
    verifier
        .verify_registration(
            registration,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            &scope,
        )
        .map_err(AuthorityProcessInventoryErrorV1::Observation)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorityProcessInventoryRestartDispositionV1 {
    QuiescentRecovery,
    SameOwnerContinuity,
}

fn same_real_process_birth(
    durable_owner: &VerifiedHostProcessIdentityV1,
    current_owner: &VerifiedHostProcessIdentityV1,
) -> bool {
    durable_owner.process_id() == current_owner.process_id()
        && durable_owner.birth_identity_hash() == current_owner.birth_identity_hash()
}

fn ensure_restart_is_safe(
    recovery_probe: Arc<dyn HostProcessIdentityRecoveryProbeV1>,
    verifier: Arc<dyn HostProcessObservationVerifierV1>,
    snapshot: &AuthorityProcessInventorySnapshotV1,
    current_owner: &VerifiedHostProcessIdentityV1,
) -> Result<AuthorityProcessInventoryRestartDispositionV1, AuthorityProcessInventoryErrorV1> {
    let owner_scope = snapshot.owner_subject.scope();
    let owner = recovery_probe
        .observe_identity_for_authority_recovery(
            &snapshot.owner_subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            owner_scope,
        )
        .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
    let owner = verifier
        .verify_recovery_observation(
            &snapshot.owner_subject,
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            owner_scope,
            owner,
        )
        .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
    let same_owner_continuity = owner.vitality == ProcessVitalityV1::Live
        && same_real_process_birth(&snapshot.owner_subject, current_owner);
    if owner.vitality == ProcessVitalityV1::Live && !same_owner_continuity {
        return Err(AuthorityProcessInventoryErrorV1::PriorOwnerStillLive);
    }
    for entry in snapshot.entries.values() {
        match &entry.state {
            AuthorityProcessInventoryStateV1::Prepared { .. } => {
                return Err(AuthorityProcessInventoryErrorV1::PreparedProcessRecoveryRequired);
            }
            AuthorityProcessInventoryStateV1::Attached { subject } => {
                let scope = subject.scope();
                let observation = recovery_probe
                    .observe_identity_for_authority_recovery(
                        subject,
                        ProcessObservationSubjectKindV1::ManagedExecution,
                        scope,
                    )
                    .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
                let observation = verifier
                    .verify_recovery_observation(
                        subject,
                        ProcessObservationSubjectKindV1::ManagedExecution,
                        scope,
                        observation,
                    )
                    .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
                if observation.vitality == ProcessVitalityV1::Live && !same_owner_continuity {
                    return Err(
                        AuthorityProcessInventoryErrorV1::PriorManagedProcessStillLive(
                            subject.registration_hash(),
                        ),
                    );
                }
            }
        }
    }
    Ok(if same_owner_continuity {
        AuthorityProcessInventoryRestartDispositionV1::SameOwnerContinuity
    } else {
        AuthorityProcessInventoryRestartDispositionV1::QuiescentRecovery
    })
}

fn read_snapshot(
    store: &AuthorityBootstrapStoreV1,
    guard: &AuthorityBootstrapPublicationGuard,
) -> Result<Option<AuthorityProcessInventorySnapshotV1>, AuthorityProcessInventoryErrorV1> {
    let Some(bytes) = store.read_bytes(guard, AuthorityBootstrapObjectClassV1::ProcessInventory)?
    else {
        return Ok(None);
    };
    decode_snapshot(&bytes).map(Some)
}

fn read_authenticator(
    store: &AuthorityBootstrapStoreV1,
    guard: &AuthorityBootstrapPublicationGuard,
) -> Result<Option<AuthorityProcessInventoryAuthenticatorV1>, AuthorityProcessInventoryErrorV1> {
    let Some(bytes) = store.read_bytes(
        guard,
        AuthorityBootstrapObjectClassV1::ProcessInventoryAuthenticator,
    )?
    else {
        return Ok(None);
    };
    decode_authenticator(&bytes).map(Some)
}

pub(crate) fn decode_authenticator(
    bytes: &[u8],
) -> Result<AuthorityProcessInventoryAuthenticatorV1, AuthorityProcessInventoryErrorV1> {
    let authenticator: AuthorityProcessInventoryAuthenticatorV1 = serde_json::from_slice(bytes)
        .map_err(|error| {
            BootstrapErrorV1::MetadataCorrupted(format!(
                "authority process inventory authenticator is malformed: {error}"
            ))
        })?;
    authenticator.validate()?;
    Ok(authenticator)
}

pub(crate) fn decode_snapshot(
    bytes: &[u8],
) -> Result<AuthorityProcessInventorySnapshotV1, AuthorityProcessInventoryErrorV1> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory is malformed: {error}"
        ))
    })?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(PROCESS_INVENTORY_SCHEMA_VERSION))
    {
        return Err(AuthorityProcessInventoryErrorV1::LegacySchema);
    }
    serde_json::from_value(value).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory is malformed: {error}"
        ))
        .into()
    })
}

fn read_requirement(
    store: &AuthorityBootstrapStoreV1,
    guard: &AuthorityBootstrapPublicationGuard,
) -> Result<Option<AuthorityProcessInventoryRequirementV1>, AuthorityProcessInventoryErrorV1> {
    let Some(bytes) = store.read_bytes(
        guard,
        AuthorityBootstrapObjectClassV1::ProcessInventoryRequirement,
    )?
    else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory requirement is malformed: {error}"
        ))
    })?;
    if value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(u64::from(PROCESS_INVENTORY_REQUIREMENT_SCHEMA_VERSION))
    {
        return Err(AuthorityProcessInventoryErrorV1::LegacySchema);
    }
    serde_json::from_value(value).map(Some).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory requirement is malformed: {error}"
        ))
        .into()
    })
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

fn publish_authenticator(
    store: &AuthorityBootstrapStoreV1,
    guard: &AuthorityBootstrapPublicationGuard,
    authenticator: &AuthorityProcessInventoryAuthenticatorV1,
) -> Result<(), AuthorityProcessInventoryErrorV1> {
    let bytes = serde_json::to_vec(authenticator).map_err(|error| {
        BootstrapErrorV1::MetadataCorrupted(format!(
            "authority process inventory authenticator serialization failed: {error}"
        ))
    })?;
    store.publish_bytes(
        guard,
        AuthorityBootstrapObjectClassV1::ProcessInventoryAuthenticator,
        &bytes,
    )?;
    Ok(())
}

fn requirement_hash(authority_epoch: u64) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"authority-process-inventory-required-v2");
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

/// In-memory test adapter for isolated unit fixtures.
///
/// It preserves the registration/verification call shape so fixture callers cannot retain a
/// PID-only attach API. It is not evidence for platform birth, durable authentication, or
/// recovery behavior; those properties are covered through the production observer/inventory
/// path in dedicated tests.
#[cfg(feature = "test-support")]
pub struct InMemoryAuthorityProcessInventoryV1 {
    entries: Mutex<BTreeMap<String, AuthorityProcessInventoryStateV1>>,
    registration_service: Arc<TestObservationServiceV1>,
    verifier: Arc<TestObservationServiceV1>,
}

#[cfg(feature = "test-support")]
impl Default for InMemoryAuthorityProcessInventoryV1 {
    fn default() -> Self {
        let service = Arc::new(TestObservationServiceV1);
        Self {
            entries: Mutex::new(BTreeMap::new()),
            registration_service: Arc::clone(&service),
            verifier: service,
        }
    }
}

#[cfg(feature = "test-support")]
impl AuthorityProcessInventoryPortV1 for InMemoryAuthorityProcessInventoryV1 {
    fn prepare_spawn(
        &self,
        request: AuthorityProcessSpawnRequestV1,
    ) -> Result<AuthorityProcessInventoryClaimV1, AuthorityProcessInventoryErrorV1> {
        let scope = request.scope(1, 1)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?;
        if entries
            .insert(
                request.attempt_id.as_str().to_owned(),
                AuthorityProcessInventoryStateV1::Prepared {
                    scope: scope.clone(),
                },
            )
            .is_some()
        {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        Ok(AuthorityProcessInventoryClaimV1 {
            attempt_id: request.attempt_id,
            scope,
            registration_service: Arc::clone(&self.registration_service)
                as Arc<dyn HostProcessObservationServiceV1>,
        })
    }

    fn attach_spawn(
        &self,
        claim: &AuthorityProcessInventoryClaimV1,
        registration: HostProcessIdentityRegistrationV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        let subject = self
            .verifier
            .verify_registration(
                registration,
                ProcessObservationSubjectKindV1::ManagedExecution,
                &claim.scope,
            )
            .map_err(AuthorityProcessInventoryErrorV1::Observation)?;
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?;
        let state = entries
            .get_mut(claim.attempt_id.as_str())
            .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
        if !matches!(state, AuthorityProcessInventoryStateV1::Prepared { scope } if scope == &claim.scope)
        {
            return Err(AuthorityProcessInventoryErrorV1::InvalidClaim);
        }
        *state = AuthorityProcessInventoryStateV1::Attached { subject };
        Ok(())
    }

    fn settle_spawn(
        &self,
        claim: AuthorityProcessInventoryClaimV1,
    ) -> Result<(), AuthorityProcessInventoryErrorV1> {
        self.entries
            .lock()
            .map_err(|_| AuthorityProcessInventoryErrorV1::LockPoisoned)?
            .remove(claim.attempt_id.as_str())
            .ok_or(AuthorityProcessInventoryErrorV1::InvalidClaim)?;
        Ok(())
    }
}

#[cfg(feature = "test-support")]
struct TestObservationServiceV1;

#[cfg(feature = "test-support")]
impl TestObservationServiceV1 {
    fn registration(
        &self,
        kind: ProcessObservationSubjectKindV1,
        scope: ProcessObservationScopeV1,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        if process_id == 0 || !scope.is_well_formed() {
            return Err(ProcessObservationErrorV1::NotObservable);
        }
        let hash = test_hash(&format!("birth:{process_id}"));
        let registration_hash = test_hash(&format!(
            "registration:{process_id}:{}",
            scope.authority_epoch
        ));
        let identity = VerifiedHostProcessIdentityV1::from_verified_registration(
            process_id,
            hash,
            kind,
            scope,
            "test-registration-nonce".to_owned(),
            registration_hash,
            test_hash("test-service"),
            1,
        );
        Ok(HostProcessIdentityRegistrationV1::new(
            identity,
            "test-issuance".to_owned(),
            0,
        ))
    }
}

#[cfg(feature = "test-support")]
impl HostProcessObservationServiceV1 for TestObservationServiceV1 {
    fn register_current_authority_owner(
        &self,
        scope: ProcessObservationScopeV1,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        self.registration(
            ProcessObservationSubjectKindV1::AuthorityBootstrapOwner,
            scope,
            std::process::id(),
        )
    }

    fn register_spawned_process(
        &self,
        scope: ProcessObservationScopeV1,
        process_id: u32,
    ) -> Result<HostProcessIdentityRegistrationV1, ProcessObservationErrorV1> {
        self.registration(
            ProcessObservationSubjectKindV1::ManagedExecution,
            scope,
            process_id,
        )
    }
}

#[cfg(feature = "test-support")]
impl HostProcessObservationVerifierV1 for TestObservationServiceV1 {
    fn verifier_instance_hash(&self) -> CanonicalHash {
        test_hash("test-service")
    }

    fn verifier_service_generation(&self) -> u64 {
        1
    }

    fn verify_registration(
        &self,
        registration: HostProcessIdentityRegistrationV1,
        expected_kind: ProcessObservationSubjectKindV1,
        expected_scope: &ProcessObservationScopeV1,
    ) -> Result<VerifiedHostProcessIdentityV1, ProcessObservationErrorV1> {
        if registration
            .identity()
            .has_exact_binding(expected_kind, expected_scope)
        {
            Ok(registration.identity().clone())
        } else if registration.identity().subject_kind() != expected_kind {
            Err(ProcessObservationErrorV1::SubjectKindMismatch)
        } else {
            Err(ProcessObservationErrorV1::ScopeMismatch)
        }
    }

    fn verify_recovery_observation(
        &self,
        _subject: &VerifiedHostProcessIdentityV1,
        _expected_kind: ProcessObservationSubjectKindV1,
        _expected_scope: &ProcessObservationScopeV1,
        _observation: sigil_kernel::process_observation::HostProcessRecoveryObservationV1,
    ) -> Result<
        sigil_kernel::process_observation::VerifiedHostProcessRecoveryObservationV1,
        ProcessObservationErrorV1,
    > {
        Err(ProcessObservationErrorV1::NotObservable)
    }
}

#[cfg(feature = "test-support")]
fn test_hash(value: &str) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    CanonicalHash::from_bytes(Sha256::digest(value.as_bytes()).into())
}
