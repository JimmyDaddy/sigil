//! RFC-0071: the single Resource Authority service factory.
//!
//! The factory exposes only current managed-file, managed-storage and borrowed-save ports plus
//! the small verification facets that still have an independent authority meaning. Resource
//! admission no longer has a coordinator or a replay-journal verifier.

use std::sync::Arc;

use crate::native_save::BorrowedNativeSaveServiceV1;
use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::AuthorityGeneration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RaOwnedVerifierKindV1 {
    StorageActivation,
    WorkspaceMutationAuthority,
}

#[derive(Debug)]
pub struct RaOwnedVerifierV1 {
    pub kind: RaOwnedVerifierKindV1,
    pub instance_hash: String,
}

/// The bounded consumer bundle returned by the unique authority factory.
pub struct ResourceAuthorityServiceBundleV1 {
    pub file_access: Arc<dyn ManagedFileAccessServiceV1>,
    pub storage: Arc<dyn ManagedStorageServiceV1>,
    pub verifiers: Vec<RaOwnedVerifierV1>,
    pub borrowed_native_save: Option<Arc<dyn BorrowedNativeSaveServiceV1>>,
}

pub struct ResourceAuthorityServiceFactoryV1 {
    authority_generation: AuthorityGeneration,
    storage: Arc<dyn ManagedStorageServiceV1>,
    file_access: Arc<dyn ManagedFileAccessServiceV1>,
    borrowed_native_save: Option<Arc<dyn BorrowedNativeSaveServiceV1>>,
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
            borrowed_native_save: None,
        }
    }

    pub fn new_with_borrowed_native_save(
        authority_generation: AuthorityGeneration,
        storage: Arc<dyn ManagedStorageServiceV1>,
        file_access: Arc<dyn ManagedFileAccessServiceV1>,
        borrowed_native_save: Arc<dyn BorrowedNativeSaveServiceV1>,
    ) -> Self {
        Self {
            authority_generation,
            storage,
            file_access,
            borrowed_native_save: Some(borrowed_native_save),
        }
    }

    pub fn authority_generation(&self) -> AuthorityGeneration {
        self.authority_generation
    }

    pub fn build_bundle(&self) -> ResourceAuthorityServiceBundleV1 {
        ResourceAuthorityServiceBundleV1 {
            file_access: Arc::clone(&self.file_access),
            storage: Arc::clone(&self.storage),
            verifiers: vec![
                RaOwnedVerifierV1 {
                    kind: RaOwnedVerifierKindV1::StorageActivation,
                    instance_hash: format!("verifier-storage-{}", self.authority_generation.epoch),
                },
                RaOwnedVerifierV1 {
                    kind: RaOwnedVerifierKindV1::WorkspaceMutationAuthority,
                    instance_hash: format!("verifier-mutation-{}", self.authority_generation.epoch),
                },
            ],
            borrowed_native_save: self.borrowed_native_save.clone(),
        }
    }
}

#[cfg(test)]
#[path = "tests/factory_tests.rs"]
mod tests;
