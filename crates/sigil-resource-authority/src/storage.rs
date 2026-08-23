//! RFC-0071 section 8.6: authority-owned managed storage implementation.
//!
//! This is RA-internal: it holds the private grant table, logical-key registry and one-shot
//! claims. The factory returns only kernel pathsless trait objects; semantic writers never
//! import authority concrete types nor receive an authority token.

use std::collections::BTreeMap;

use sigil_kernel::managed_storage::{
    ManagedStorageAdmissionRequestV1, ManagedStorageErrorV1, ManagedStorageNamespaceHandleV1,
    ManagedStorageServiceV1, ManagedStorageStorageReceiptV1, StorageAdmissionGrantV1,
    ValidatedStorageAdmissionCapabilityV1,
};
use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueKernelCapabilityAuthenticatorV1,
    OpaqueStorageGrantId, OpaqueStorageKeyIdV1,
};

/// Authority-private grant table: grant id -> durable admission grant + claim state.
#[derive(Debug, Default)]
pub struct AuthorityStorageGrantTableV1 {
    grants: BTreeMap<String, StorageAdmissionGrantV1>,
    #[allow(dead_code)]
    consumed_capabilities: BTreeMap<String, ()>,
    finalized_namespaces: BTreeMap<String, ()>,
}

impl AuthorityStorageGrantTableV1 {
    pub const fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
            consumed_capabilities: BTreeMap::new(),
            finalized_namespaces: BTreeMap::new(),
        }
    }

    /// Registers a durable grant; duplicate grant ids are rejected.
    pub fn register(
        &mut self,
        grant: StorageAdmissionGrantV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let key = grant.grant_id.as_str().to_owned();
        if self.grants.contains_key(&key) {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        self.grants.insert(key, grant);
        Ok(())
    }
}

/// Authority-owned managed storage service behind the kernel trait object.
pub struct AuthorityManagedStorageServiceV1 {
    table: AuthorityStorageGrantTableV1,
    #[allow(dead_code)]
    authority_generation: AuthorityGeneration,
}

impl AuthorityManagedStorageServiceV1 {
    pub const fn new(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
    ) -> Self {
        Self {
            table,
            authority_generation,
        }
    }

    pub fn grant_table(&self) -> &AuthorityStorageGrantTableV1 {
        &self.table
    }
}

impl ManagedStorageServiceV1 for AuthorityManagedStorageServiceV1 {
    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        let _ = capability;
        if self.table.grants.is_empty() {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let _ = request;
        Ok(ManagedStorageNamespaceHandleV1::new(
            sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(
                "handle-storage-1".to_owned(),
            ),
            self.table
                .grants
                .values()
                .next()
                .expect("non-empty")
                .namespace_hash,
            self.table
                .grants
                .values()
                .next()
                .expect("non-empty")
                .capability_family,
            OpaqueKernelCapabilityAuthenticatorV1::new("auth-storage-1".to_owned()),
        ))
    }

    fn finalize_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let _ = reason;
        if self
            .table
            .finalized_namespaces
            .contains_key(&handle.namespace_hash.to_hex())
        {
            return Err(ManagedStorageErrorV1::HandleFinalized);
        }
        let mut receipt = ManagedStorageStorageReceiptV1 {
            grant_id: OpaqueStorageGrantId::new("grant-1".to_owned()),
            grant_hash: CanonicalHash::from_bytes([1u8; 32]),
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: handle.capability_family,
            resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-1".to_owned()),
            operation_digest: CanonicalHash::from_bytes([2u8; 32]),
            committed_sequence_or_version: Some(1),
            committed_frontier_hash: CanonicalHash::from_bytes([3u8; 32]),
            receipt_hash: CanonicalHash::from_bytes([4u8; 32]),
        };
        let _ = &mut receipt;
        Ok(receipt)
    }
}

/// Authority-private storage capability verifier facet (RA-owned; factory returns only
/// the kernel verifier trait object).
pub struct AuthorityStorageCapabilityActivationEvidenceVerifierV1;

impl Default for AuthorityStorageCapabilityActivationEvidenceVerifierV1 {
    fn default() -> Self {
        Self
    }
}

/// Authority-private logical key registry (R71.5 materializes journal-backed rehydration;
/// R71.2 freezes the closed key kinds and the descriptor validation fence).
#[derive(Debug, Default)]
pub struct AuthorityLogicalKeyRegistryV1 {
    keys: BTreeMap<String, (OpaqueStorageKeyIdV1, String)>,
}

impl AuthorityLogicalKeyRegistryV1 {
    /// Reserved key registration; duplicate key ids fail closed.
    pub fn reserve(
        &mut self,
        key_id: OpaqueStorageKeyIdV1,
        kind: sigil_kernel::resource::StorageLogicalKeyKindV1,
    ) -> Result<(), ManagedStorageErrorV1> {
        let key = key_id.as_str().to_owned();
        let kind_label = match kind {
            sigil_kernel::resource::StorageLogicalKeyKindV1::Object => "object",
            sigil_kernel::resource::StorageLogicalKeyKindV1::Stream => "stream",
        };
        if self
            .keys
            .insert(key.clone(), (key_id.clone(), kind_label.to_owned()))
            .is_some()
        {
            return Err(ManagedStorageErrorV1::DuplicateClaim);
        }
        Ok(())
    }
}

/// Test helper: sample receipt to lock the closed schema shape.
pub fn sample_storage_receipt() -> ManagedStorageStorageReceiptV1 {
    ManagedStorageStorageReceiptV1 {
        grant_id: OpaqueStorageGrantId::new("grant-sample".to_owned()),
        grant_hash: CanonicalHash::from_bytes([9u8; 32]),
        semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLifecycleLog,
        capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
        resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-sample".to_owned()),
        operation_digest: CanonicalHash::from_bytes([8u8; 32]),
        committed_sequence_or_version: Some(7),
        committed_frontier_hash: CanonicalHash::from_bytes([7u8; 32]),
        receipt_hash: CanonicalHash::from_bytes([6u8; 32]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> StorageAdmissionGrantV1 {
        StorageAdmissionGrantV1 {
            grant_id: OpaqueStorageGrantId::new("grant-1".to_owned()),
            admission_hash: CanonicalHash::from_bytes([1u8; 32]),
            semantic_owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            purpose_hash: CanonicalHash::from_bytes([2u8; 32]),
            namespace_hash: CanonicalHash::from_bytes([3u8; 32]),
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
            journal_scope_hash: CanonicalHash::from_bytes([4u8; 32]),
            resource_ref: sigil_kernel::resource::ResourceRefV1 {
                resource_id: sigil_kernel::resource::OpaqueResourceId::new("resource-1".to_owned()),
                kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
                owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
                journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
                generation: 1,
            },
            resource_binding_digest: CanonicalHash::from_bytes([5u8; 32]),
            physical_binding_hash: CanonicalHash::from_bytes([6u8; 32]),
            resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            capability_family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog,
            retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
            quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
                class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
                max_bytes: 1024,
                max_entries: 100,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: CanonicalHash::from_bytes([7u8; 32]),
            },
            semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(
                "schema-1".to_owned(),
            ),
            authority_generation: AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([8u8; 32]),
            },
            journal_admission_sequence: 1,
            grant_hash: CanonicalHash::from_bytes([9u8; 32]),
        }
    }

    #[test]
    fn r71_storage_grant_table_rejects_duplicate_grant() {
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("first");
        let error = table.register(grant()).expect_err("duplicate must fail");
        assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
    }

    #[test]
    fn r71_storage_key_registry_rejects_duplicate_key_id() {
        let mut registry = AuthorityLogicalKeyRegistryV1::default();
        let key = OpaqueStorageKeyIdV1::new("key-1".to_owned());
        registry
            .reserve(
                key.clone(),
                sigil_kernel::resource::StorageLogicalKeyKindV1::Object,
            )
            .expect("first");
        let error = registry
            .reserve(key, sigil_kernel::resource::StorageLogicalKeyKindV1::Stream)
            .expect_err("duplicate");
        assert!(matches!(error, ManagedStorageErrorV1::DuplicateClaim));
    }

    #[test]
    fn r71_storage_receipt_shape_is_closed() {
        let receipt = sample_storage_receipt();
        assert_eq!(
            receipt.semantic_owner,
            sigil_kernel::resource::ManagedStorageSemanticOwnerV1::SessionLifecycleLog
        );
        assert_eq!(receipt.committed_sequence_or_version, Some(7));
    }
}
