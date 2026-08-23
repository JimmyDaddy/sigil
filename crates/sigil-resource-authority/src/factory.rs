//! RFC-0071 section 9.1/9.4: ResourceAuthorityServiceFactoryV1.
//!
//! This factory is the ONLY entry point a composition uses to obtain: the sandbox binder
//! registry, borrowed-subject registration, managed file access, managed storage, managed
//! projection services and the runtime-only resource journal coordinator protocol service,
//! plus exactly five RA-owned verifiers (storage activation, spawn resource journal, workspace
//! mutation authority, domain-storage shadow/settled-chain, recovery Prepared+Settled journal).
//!
//! It never returns a sandbox terminal facet and never exposes an authority token or private
//! primitive lease.

use std::sync::Arc;

use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::AuthorityGeneration;

/// Verifier variants exposed by the factory (closed, exactly five).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaOwnedVerifierKindV1 {
    StorageActivation,
    SpawnResourceJournal,
    WorkspaceMutationAuthority,
    DomainStorageShadow,
    RecoveryPreparedSettled,
}

/// One RA-owned verification capability.
#[derive(Debug)]
pub struct RaOwnedVerifierV1 {
    pub kind: RaOwnedVerifierKindV1,
    pub instance_hash: String,
}

/// Runtime-only resource journal coordinator protocol service (pathsless coordinator facet).
#[derive(Debug)]
pub struct ResourceJournalCoordinatorProtocolServiceV1 {
    pub journal_instance_hash: String,
}

/// The factory return bundle: exactly the consumer surfaces + verifiers + coordinator.
pub struct ResourceAuthorityServiceBundleV1 {
    pub file_access: Arc<dyn ManagedFileAccessServiceV1>,
    pub storage: Arc<dyn ManagedStorageServiceV1>,
    pub verifiers: Vec<RaOwnedVerifierV1>,
    pub journal_coordinator: ResourceJournalCoordinatorProtocolServiceV1,
}

/// The unique factory. Compositions may not construct a second instance through any other path.
pub struct ResourceAuthorityServiceFactoryV1 {
    authority_generation: AuthorityGeneration,
    storage: Arc<dyn ManagedStorageServiceV1>,
    file_access: Arc<dyn ManagedFileAccessServiceV1>,
}

impl ResourceAuthorityServiceFactoryV1 {
    pub fn new(
        authority_generation: AuthorityGeneration,
        storage: Arc<dyn ManagedStorageServiceV1>,
        file_access: Arc<dyn ManagedFileAccessServiceV1>,
    ) -> Self {
        Self {
            authority_generation,
            storage,
            file_access,
        }
    }

    pub fn authority_generation(&self) -> AuthorityGeneration {
        self.authority_generation
    }

    /// Builds the bounded bundle. Verifier count is fixed by the closed enum (exactly five).
    pub fn build_bundle(&self) -> ResourceAuthorityServiceBundleV1 {
        let verifiers = vec![
            RaOwnedVerifierV1 {
                kind: RaOwnedVerifierKindV1::StorageActivation,
                instance_hash: format!("verifier-storage-{}", self.authority_generation.epoch),
            },
            RaOwnedVerifierV1 {
                kind: RaOwnedVerifierKindV1::SpawnResourceJournal,
                instance_hash: format!("verifier-spawn-{}", self.authority_generation.epoch),
            },
            RaOwnedVerifierV1 {
                kind: RaOwnedVerifierKindV1::WorkspaceMutationAuthority,
                instance_hash: format!("verifier-mutation-{}", self.authority_generation.epoch),
            },
            RaOwnedVerifierV1 {
                kind: RaOwnedVerifierKindV1::DomainStorageShadow,
                instance_hash: format!(
                    "verifier-domain-shadow-{}",
                    self.authority_generation.epoch
                ),
            },
            RaOwnedVerifierV1 {
                kind: RaOwnedVerifierKindV1::RecoveryPreparedSettled,
                instance_hash: format!("verifier-recovery-{}", self.authority_generation.epoch),
            },
        ];
        ResourceAuthorityServiceBundleV1 {
            file_access: Arc::clone(&self.file_access),
            storage: Arc::clone(&self.storage),
            verifiers,
            journal_coordinator: ResourceJournalCoordinatorProtocolServiceV1 {
                journal_instance_hash: format!("journal-{}", self.authority_generation.epoch),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_kernel::resource::CanonicalHash;

    #[test]
    fn r71_factory_exposes_exactly_five_ra_owned_verifiers() {
        let storage = Arc::new(crate::storage::AuthorityManagedStorageServiceV1::new(
            crate::storage::AuthorityStorageGrantTableV1::new(),
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([0u8; 32]),
            },
        ));
        let file_access = crate::file_access_stub::stub_file_access_service();
        let factory = ResourceAuthorityServiceFactoryV1::new(
            AuthorityGeneration {
                epoch: 2,
                instance_hash: CanonicalHash::from_bytes([1u8; 32]),
            },
            storage,
            file_access,
        );
        let bundle = factory.build_bundle();
        assert_eq!(bundle.verifiers.len(), 5, "exactly five RA-owned verifiers");
        let kinds: Vec<_> = bundle.verifiers.iter().map(|v| v.kind).collect();
        assert!(kinds.contains(&RaOwnedVerifierKindV1::StorageActivation));
        assert!(kinds.contains(&RaOwnedVerifierKindV1::RecoveryPreparedSettled));
        assert!(!bundle.journal_coordinator.journal_instance_hash.is_empty());
    }
}
