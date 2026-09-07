use super::*;
use sigil_kernel::managed_storage::{ManagedStorageServiceV1, StorageAdmissionGrantV1};
use sigil_kernel::resource::{
    AuthorityGeneration, ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
    OpaqueStorageGrantId, ResourceAuthorityScopeV1, ResourceOwnerScopeV1,
};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};

fn hash(seed: u8) -> CanonicalHash {
    CanonicalHash::from_bytes([seed; 32])
}

fn session_log_grant() -> StorageAdmissionGrantV1 {
    let source = sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
        cutover_manifest_hash: hash(10),
        application_generation: 1,
    };
    StorageAdmissionGrantV1 {
        grant_id: OpaqueStorageGrantId::new("g-writer-slog".to_owned()),
        admission_hash: hash(1),
        semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: hash(2),
        source_class: sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: sigil_resource_authority::storage::admission_source_binding_hash(
            &source,
        ),
        namespace_hash: writer_namespace_hash("session-log"),
        authority_scope: ResourceAuthorityScopeV1::Application,
        authority_scope_hash: hash(4),
        resource_ref: sigil_kernel::resource::ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new(
                "res-writer-slog".to_owned(),
            ),
            kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: ResourceOwnerScopeV1::Application,
            authority_scope: ResourceAuthorityScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: hash(5),
        physical_binding_hash: hash(6),
        resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
        owner_scope: ResourceOwnerScopeV1::Application,
        capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
        retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: hash(7),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
            "schema-writer-slog".to_owned(),
        ),
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: hash(8),
        },
        grant_hash: hash(9),
    }
}

fn adapter(anchor: &std::path::Path) -> ManagedStorageWriterAdapterV1 {
    let mut table = AuthorityStorageGrantTableV1::new();
    table.register(session_log_grant()).expect("grant");
    let service: std::sync::Arc<dyn ManagedStorageServiceV1> =
        std::sync::Arc::new(AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
        ));
    ManagedStorageWriterAdapterV1::new(service, anchor.to_path_buf(), hash(10))
}

#[test]
fn writer_batch_uses_stable_hashed_namespace_and_current_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = adapter(dir.path());
    let lease = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "session-1")
        .expect("acquire");
    assert!(lease.path().ends_with(lease.namespace_digest().to_hex()));
    writer.write_record(&lease, b"{\"seq\":1}").expect("write");
    let marker: serde_json::Value = serde_json::from_slice(
        &std::fs::read(lease.path().join("authority-admission.json")).expect("marker"),
    )
    .expect("marker json");
    assert_eq!(marker["schema_version"], 3);
    assert_eq!(
        marker["namespace_hash"],
        serde_json::to_value(lease.namespace_digest()).expect("namespace digest must serialize")
    );
    writer.finalize(lease).expect("finalize");
}

#[test]
fn writer_rejects_unsafe_named_key_before_filesystem_access() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = adapter(dir.path());
    let error = writer
        .acquire_named(StorageWriterChannelV1::SessionLog, "../escape")
        .expect_err("unsafe key");
    assert!(matches!(
        error,
        ManagedStorageWriterErrorV1::LeafEscapesAnchor
    ));
}

#[test]
fn writer_rejects_mutation_after_settlement() {
    let dir = tempfile::tempdir().expect("tempdir");
    let writer = adapter(dir.path());
    let lease = writer
        .acquire(StorageWriterChannelV1::SessionLog)
        .expect("acquire");
    writer.finalize(lease).expect("finalize");
}

#[test]
fn physical_record_frontier_accepts_jsonl_and_rejects_torn_tail() {
    assert_eq!(
        managed_physical_record_count(StorageWriterChannelV1::SessionLog, b"{}\n{}\n")
            .expect("jsonl"),
        2
    );
    assert!(managed_physical_record_count(StorageWriterChannelV1::SessionLog, b"{}").is_err());
}
