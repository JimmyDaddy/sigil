use std::sync::Arc;

#[test]
fn factory_exposes_only_current_authority_facets() {
    let generation = sigil_kernel::resource::AuthorityGeneration {
        epoch: 1,
        instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([1; 32]),
    };
    let storage = Arc::new(crate::storage::AuthorityManagedStorageServiceV1::new(
        crate::storage::AuthorityStorageGrantTableV1::new(),
        generation,
    ));
    let file_access = Arc::new(
        crate::file_access::AuthorityManagedFileAccessServiceV1::new(Arc::new(
            std::sync::Mutex::new(crate::borrowed::BorrowedSubjectRegistryV1::new()),
        )),
    );
    let bundle =
        crate::factory::ResourceAuthorityServiceFactoryV1::new(generation, storage, file_access)
            .build_bundle();
    assert_eq!(bundle.verifiers.len(), 2);
    assert!(
        bundle
            .verifiers
            .iter()
            .any(|verifier| verifier.kind
                == crate::factory::RaOwnedVerifierKindV1::StorageActivation)
    );
}
