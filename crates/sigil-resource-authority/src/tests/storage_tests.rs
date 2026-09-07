use super::*;
use sigil_kernel::managed_storage::{ManagedStorageServiceV1, StorageAdmissionSourceV1};
use sigil_kernel::resource::{
    ManagedStorageAdmissionPurposeV1, ManagedStorageCapabilityFamilyV1, ResourceAuthorityScopeV1,
    ResourceKindV1, ResourceOwnerScopeV1, ResourceQuotaClassV1, ResourceRefV1,
    ResourceRetentionPolicyV1, StorageAdmissionSourceClassV1,
};

fn source() -> StorageAdmissionSourceV1 {
    StorageAdmissionSourceV1::ApplicationCutoverRoot {
        cutover_manifest_hash: CanonicalHash::from_bytes([0x11; 32]),
        application_generation: 1,
    }
}

fn authority() -> AuthorityGeneration {
    AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x12; 32]),
    }
}

fn grant() -> StorageAdmissionGrantV1 {
    StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new("grant-storage-test".to_owned()),
        admission_hash: CanonicalHash::from_bytes([1; 32]),
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        purpose: ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: CanonicalHash::from_bytes([2; 32]),
        source_class: StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: admission_source_binding_hash(&source()),
        namespace_hash: CanonicalHash::from_bytes([3; 32]),
        authority_scope: ResourceAuthorityScopeV1::Application,
        authority_scope_hash: CanonicalHash::from_bytes([4; 32]),
        resource_ref: ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new(
                "resource-storage-test".to_owned(),
            ),
            kind: ResourceKindV1::RuntimeState,
            owner_scope: ResourceOwnerScopeV1::Application,
            authority_scope: ResourceAuthorityScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: CanonicalHash::from_bytes([5; 32]),
        physical_binding_hash: CanonicalHash::from_bytes([6; 32]),
        resource_kind: ResourceKindV1::RuntimeState,
        owner_scope: ResourceOwnerScopeV1::Application,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        retention_policy: ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: CanonicalHash::from_bytes([7; 32]),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
            "schema-storage-test".to_owned(),
        ),
        authority_generation: authority(),
        grant_hash: CanonicalHash::from_bytes([8; 32]),
    }
}

fn request(namespace_key_hash: CanonicalHash) -> ManagedStorageAdmissionRequestV1 {
    ManagedStorageAdmissionRequestV1 {
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        purpose: ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: source(),
        owner_scope: ResourceOwnerScopeV1::Application,
        authority_scope: ResourceAuthorityScopeV1::Application,
        namespace_key_hash,
    }
}

#[test]
fn current_grant_table_rejects_duplicate_grants() {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("first registration");
    assert!(matches!(
        table.register(grant()),
        Err(ManagedStorageErrorV1::CapabilityMismatch)
    ));
}

#[test]
fn logical_key_registry_rejects_duplicate_key_ids() {
    let mut registry = AuthorityLogicalKeyRegistryV1::default();
    let key = OpaqueStorageKeyIdV1::new("key-storage-test".to_owned());
    registry
        .reserve(
            key.clone(),
            sigil_kernel::resource::StorageLogicalKeyKindV1::Object,
        )
        .expect("first key");
    assert!(matches!(
        registry.reserve(key, sigil_kernel::resource::StorageLogicalKeyKindV1::Stream),
        Err(ManagedStorageErrorV1::DuplicateClaim)
    ));
}

#[test]
fn admission_is_current_state_only_and_settles_once() {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(grant()).expect("grant");
    let service = AuthorityManagedStorageServiceV1::new(table, authority());
    let namespace = CanonicalHash::from_bytes([0x55; 32]);
    let handle = service
        .admit_namespace(
            request(namespace),
            ValidatedStorageAdmissionCapabilityV1::startup_probe(),
        )
        .expect("admit");
    service
        .validate_namespace_write(&handle)
        .expect("live lease");
    let receipt = service
        .finalize_namespace_with_physical_frontier(
            handle,
            12,
            1,
            CanonicalHash::from_bytes([0x56; 32]),
            "test-settlement".to_owned(),
        )
        .expect("settle");
    assert_eq!(receipt.committed_entry_count, Some(1));
    assert!(matches!(
        service.validate_namespace_write(&ManagedStorageNamespaceHandleV1::new(
            OpaqueKernelCapabilityHandleId::new("handle-probe-storage-2".to_owned()),
            namespace,
            ManagedStorageCapabilityFamilyV1::AppendLog,
            OpaqueKernelCapabilityAuthenticatorV1::new("test".to_owned()),
        )),
        Err(ManagedStorageErrorV1::HandleFinalized)
    ));
}

#[test]
fn sample_receipt_keeps_physical_observation_optional() {
    let receipt = sample_storage_receipt();
    assert_eq!(
        receipt.semantic_owner,
        ManagedStorageSemanticOwnerV1::SessionLifecycleLog
    );
    assert!(receipt.physical_frontier_hash.is_none());
}
