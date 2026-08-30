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
