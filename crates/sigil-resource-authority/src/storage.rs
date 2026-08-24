//! RFC-0071 section 8.6: authority-owned managed storage implementation.
//!
//! This is RA-internal: it holds the private grant table, logical-key registry and one-shot
//! claims. The factory returns only kernel pathsless trait objects; semantic writers never
//! import authority concrete types nor receive an authority token.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;

use sigil_kernel::managed_storage::{
    ManagedStorageAdmissionRequestV1, ManagedStorageErrorV1, ManagedStorageNamespaceHandleV1,
    ManagedStorageServiceV1, ManagedStorageStorageReceiptV1, StorageAdmissionGrantV1,
    ValidatedStorageAdmissionCapabilityV1,
};
use sigil_kernel::resource::{
    AuthorityGeneration, CanonicalHash, OpaqueKernelCapabilityAuthenticatorV1,
    OpaqueStorageGrantId, OpaqueStorageKeyIdV1,
};

use crate::journal::{
    JournalErrorV1, ResourceJournalEventV1, ResourceJournalFileV1, ResourceJournalRecordV1,
};

/// Authority-private grant table: grant id -> durable admission grant + claim state.
#[derive(Debug, Default)]
pub struct AuthorityStorageGrantTableV1 {
    grants: BTreeMap<String, StorageAdmissionGrantV1>,
    #[allow(dead_code)]
    consumed_capabilities: BTreeMap<String, ()>,
    /// Finalized namespace registry: one-shot per namespace (interior mutability because
    /// finalize is &self on the kernel port).
    finalized_namespaces: std::sync::Mutex<BTreeMap<String, ()>>,
    /// Exact admitted request and grant, retained until the one-shot finalize CAS.
    admitted_namespaces: std::sync::Mutex<BTreeMap<String, StorageAdmissionRecordV1>>,
    /// Probe-claim sequence: every kernel-owned startup-probe claim gets a distinct probe
    /// namespace so probes and shadow writer claims never share a finalized namespace.
    probe_sequence: std::sync::atomic::AtomicU64,
}

impl AuthorityStorageGrantTableV1 {
    pub const fn new() -> Self {
        Self {
            grants: BTreeMap::new(),
            consumed_capabilities: BTreeMap::new(),
            finalized_namespaces: std::sync::Mutex::new(BTreeMap::new()),
            admitted_namespaces: std::sync::Mutex::new(BTreeMap::new()),
            probe_sequence: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Next distinct probe namespace sequence (descendant proof: probes never share ns).
    fn next_probe_sequence(&self) -> u64 {
        self.probe_sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    }

    /// Mark the namespace bound to `handle` finalized exactly once.
    fn record_finalized(
        &self,
        namespace_hash: &CanonicalHash,
    ) -> Result<(), ManagedStorageErrorV1> {
        let key = namespace_hash.to_hex();
        let mut finalized = self
            .finalized_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::HandleFinalized)?;
        if finalized.contains_key(&key) {
            return Err(ManagedStorageErrorV1::HandleFinalized);
        }
        finalized.insert(key, ());
        Ok(())
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

#[derive(Debug, Clone)]
struct StorageAdmissionRecordV1 {
    grant: StorageAdmissionGrantV1,
    request: ManagedStorageAdmissionRequestV1,
    namespace_hash: CanonicalHash,
    admission_sequence: u64,
}

/// Authority-owned managed storage service behind the kernel trait object.
pub struct AuthorityManagedStorageServiceV1 {
    table: AuthorityStorageGrantTableV1,
    authority_generation: AuthorityGeneration,
    journal: Option<Mutex<ResourceJournalFileV1>>,
    blocked_grants_after_restart: BTreeMap<String, ()>,
}

impl AuthorityManagedStorageServiceV1 {
    pub const fn new(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
    ) -> Self {
        Self {
            table,
            authority_generation,
            journal: None,
            blocked_grants_after_restart: BTreeMap::new(),
        }
    }

    /// Creates the production service with an owner-only durable authority journal.
    pub fn new_with_journal(
        table: AuthorityStorageGrantTableV1,
        authority_generation: AuthorityGeneration,
        journal_path: impl AsRef<Path>,
        bootstrap_manifest_hash: CanonicalHash,
        journal_instance_hash: CanonicalHash,
    ) -> Result<Self, JournalErrorV1> {
        let header = crate::journal::ResourceJournalHeaderV1 {
            schema_version: 1,
            shard_name: "application-resources".to_owned(),
            bootstrap_manifest_hash,
            journal_instance_hash,
            header_hash: hash_debug(&(
                "application-resources",
                bootstrap_manifest_hash,
                journal_instance_hash,
            )),
        };
        let journal = ResourceJournalFileV1::open(journal_path.as_ref().to_path_buf(), header)?;
        let blocked_grants_after_restart = journal
            .unsettled_storage_grants()
            .into_iter()
            .map(|key| (key, ()))
            .collect();
        Ok(Self {
            table,
            authority_generation,
            journal: Some(Mutex::new(journal)),
            blocked_grants_after_restart,
        })
    }

    pub fn grant_table(&self) -> &AuthorityStorageGrantTableV1 {
        &self.table
    }

    fn append_journal_event(
        &self,
        event: ResourceJournalEventV1,
    ) -> Result<Option<ResourceJournalRecordV1>, ManagedStorageErrorV1> {
        let Some(journal) = &self.journal else {
            return Ok(None);
        };
        journal
            .lock()
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)?
            .append_event(event)
            .map(Some)
            .map_err(|_| ManagedStorageErrorV1::JournalUnavailable)
    }
}

impl ManagedStorageServiceV1 for AuthorityManagedStorageServiceV1 {
    fn admit_namespace(
        &self,
        request: ManagedStorageAdmissionRequestV1,
        capability: ValidatedStorageAdmissionCapabilityV1,
    ) -> Result<ManagedStorageNamespaceHandleV1, ManagedStorageErrorV1> {
        // Kernel-owned startup readiness probes use a dedicated probe namespace: the probe
        // must never finalize (consume) the production grant namespace, or the next writer
        // batch for that channel would be refused as already finalized.
        let probe = capability.handle_id.as_str() == "startup-probe";
        // Family-exact closure: any unrelated grant must not masquerade as readiness for a
        // different capability family (R71.6 cutover probe depends on this exactness).
        let Some(grant) = self.table.grants.values().find(|grant| {
            grant.capability_family == request.capability_family
                && grant.semantic_owner == request.semantic_owner
                && grant.purpose == request.purpose
                && grant.journal_scope == request.journal_scope
                && (probe || grant.owner_scope == request.owner_scope)
                && (probe || grant.source_class == request.source.source_class())
        }) else {
            return Err(ManagedStorageErrorV1::FamilyMismatch);
        };
        let binding = capability.binding();
        if !probe {
            if self
                .blocked_grants_after_restart
                .contains_key(&grant.grant_hash.to_hex())
            {
                return Err(ManagedStorageErrorV1::JournalUnavailable);
            }
            let Some(binding) = binding else {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            };
            if binding.family() != request.capability_family
                || binding.namespace_hash() != grant.namespace_hash
            {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
            if grant.source_binding_hash != source_binding_hash(&request.source)
                || grant.authority_generation != self.authority_generation
            {
                return Err(ManagedStorageErrorV1::CapabilityMismatch);
            }
        }
        let (handle_id, namespace_hash, authenticator) = if probe {
            let mut probe_ns = [0x9fu8; 32];
            let seq = self.table.next_probe_sequence();
            probe_ns[24..].copy_from_slice(&seq.to_be_bytes());
            probe_ns[0] = match grant.capability_family {
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AppendLog => 1,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::AtomicObject => 2,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection => 3,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::StreamingArtifact => 4,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::ArtifactStore => 5,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection => 6,
                sigil_kernel::resource::ManagedStorageCapabilityFamilyV1::SemanticLeaseLedger => 7,
            };
            (
                sigil_kernel::resource::OpaqueKernelCapabilityHandleId::new(format!(
                    "handle-probe-storage-{seq}"
                )),
                CanonicalHash::from_bytes(probe_ns),
                OpaqueKernelCapabilityAuthenticatorV1::new(format!("auth-probe-storage-{seq}")),
            )
        } else {
            // Broker-issued claims: the namespace identity is the claim binding itself (the
            // broker seals the true family/namespace kernel-side). Each claim is a distinct
            // one-shot namespace, so named writer batches never share a finalize scope.
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(capability.handle_id.as_str().as_bytes());
            let claim_ns = CanonicalHash::from_bytes(hasher.finalize().into());
            (
                capability.handle_id.clone(),
                claim_ns,
                OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                    "auth-{}",
                    capability.handle_id.as_str()
                )),
            )
        };
        let handle = ManagedStorageNamespaceHandleV1::new(
            handle_id,
            namespace_hash,
            grant.capability_family,
            authenticator,
        );
        let admission_sequence = self
            .append_journal_event(ResourceJournalEventV1::StorageNamespaceAdmitted {
                grant_hash: grant.grant_hash,
            })?
            .map(|record| record.sequence)
            .unwrap_or(grant.journal_admission_sequence);
        self.table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::CapabilityMismatch)?
            .insert(
                handle.handle_id.as_str().to_owned(),
                StorageAdmissionRecordV1 {
                    grant: grant.clone(),
                    request,
                    namespace_hash,
                    admission_sequence,
                },
            );
        Ok(handle)
    }

    fn finalize_namespace(
        &self,
        handle: ManagedStorageNamespaceHandleV1,
        reason: String,
    ) -> Result<ManagedStorageStorageReceiptV1, ManagedStorageErrorV1> {
        let record = self
            .table
            .admitted_namespaces
            .lock()
            .map_err(|_| ManagedStorageErrorV1::HandleFinalized)?
            .remove(handle.handle_id.as_str())
            .ok_or(ManagedStorageErrorV1::CapabilityMismatch)?;
        if record.namespace_hash != handle.namespace_hash
            || record.grant.capability_family != handle.capability_family
        {
            return Err(ManagedStorageErrorV1::CapabilityMismatch);
        }
        let operation_digest = hash_debug(&(record.request, &reason));
        let settlement = self.append_journal_event(ResourceJournalEventV1::GenerationSettled {
            grant_hash: record.grant.grant_hash,
            resource_id: record.grant.resource_ref.resource_id.as_str().to_owned(),
            generation: record.grant.resource_ref.generation,
            cleanup_status: reason,
        })?;
        self.table.record_finalized(&handle.namespace_hash)?;
        let committed_frontier_hash = if let Some(settlement) = &settlement {
            settlement.committed_frontier_hash
        } else {
            hash_debug(&(
                record.grant.grant_hash,
                record.namespace_hash,
                operation_digest,
            ))
        };
        let receipt_hash = hash_debug(&(
            record.grant.grant_id.as_str(),
            record.grant.grant_hash,
            record.grant.resource_ref.resource_id.as_str(),
            operation_digest,
            committed_frontier_hash,
            record.admission_sequence,
            settlement.as_ref().map(|record| record.sequence),
        ));
        Ok(ManagedStorageStorageReceiptV1 {
            grant_id: record.grant.grant_id,
            grant_hash: record.grant.grant_hash,
            semantic_owner: record.grant.semantic_owner,
            capability_family: record.grant.capability_family,
            resource_id: record.grant.resource_ref.resource_id,
            operation_digest,
            committed_sequence_or_version: Some(
                settlement
                    .as_ref()
                    .map(|record| record.sequence)
                    .unwrap_or(record.grant.journal_admission_sequence),
            ),
            committed_frontier_hash,
            receipt_hash,
        })
    }
}

fn source_binding_hash(
    source: &sigil_kernel::managed_storage::StorageAdmissionSourceV1,
) -> CanonicalHash {
    match source {
        sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash,
            ..
        } => *cutover_manifest_hash,
        _ => hash_debug(source),
    }
}

fn hash_debug(value: &impl std::fmt::Debug) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("{value:?}").as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
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
            source_class:
                sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
            source_binding_hash: CanonicalHash::from_bytes([9u8; 32]),
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

    #[test]
    fn r71_storage_family_exact_closure() {
        use sigil_kernel::managed_storage::{
            ManagedStorageAdmissionRequestV1, ValidatedStorageAdmissionCapabilityV1,
        };
        use sigil_kernel::resource::{
            ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1, OpaqueSessionId,
            ResourceJournalScopeV1, ResourceOwnerScopeV1,
        };
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("register");
        let service = AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([8u8; 32]),
            },
        );
        let exact = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        };
        service
            .admit_namespace(
                exact,
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect("exact family+owner");
        // Same family but different semantic owner: refused (a different writer must not piggyback).
        let piggyback = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::InteractiveInputHistory,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let error = service
            .admit_namespace(
                piggyback,
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect_err("piggyback");
        assert!(matches!(error, ManagedStorageErrorV1::FamilyMismatch));
        // Unrelated family: refused, never a masqueraded readiness.
        let unrelated = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionCatalog,
            capability_family: ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
            purpose:
                sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::RebuildableProjection,
            source:
                sigil_kernel::managed_storage::StorageAdmissionSourceV1::ApplicationCutoverRoot {
                    cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                    application_generation: 1,
                },
            owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new("s-1".to_owned())),
            journal_scope: ResourceJournalScopeV1::Application,
        };
        let error = service
            .admit_namespace(
                unrelated,
                ValidatedStorageAdmissionCapabilityV1::startup_probe(),
            )
            .expect_err("unrelated");
        assert!(matches!(error, ManagedStorageErrorV1::FamilyMismatch));
    }

    #[test]
    fn r71_storage_broker_binding_rejects_namespace_and_family_drift() {
        use sigil_kernel::capability_issuer::KernelCapabilityBrokerV1;
        use sigil_kernel::managed_storage::StorageAdmissionSourceV1;
        use sigil_kernel::resource::{
            ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1,
        };
        let mut table = AuthorityStorageGrantTableV1::new();
        table.register(grant()).expect("register");
        let service = AuthorityManagedStorageServiceV1::new(
            table,
            AuthorityGeneration {
                epoch: 1,
                instance_hash: CanonicalHash::from_bytes([8u8; 32]),
            },
        );
        let request = ManagedStorageAdmissionRequestV1 {
            semantic_owner: ManagedStorageSemanticOwnerV1::SessionLog,
            capability_family: ManagedStorageCapabilityFamilyV1::AppendLog,
            purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
            source: StorageAdmissionSourceV1::ApplicationCutoverRoot {
                cutover_manifest_hash: CanonicalHash::from_bytes([9u8; 32]),
                application_generation: 1,
            },
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        };
        let broker = KernelCapabilityBrokerV1::new();
        let exact = broker
            .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
                ManagedStorageCapabilityFamilyV1::AppendLog,
                CanonicalHash::from_bytes([3u8; 32]),
            ))
            .expect("exact capability");
        service
            .admit_namespace(request.clone(), exact)
            .expect("exact binding");
        let wrong_namespace = broker
            .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
                ManagedStorageCapabilityFamilyV1::AppendLog,
                CanonicalHash::from_bytes([4u8; 32]),
            ))
            .expect("wrong namespace capability");
        let error = service
            .admit_namespace(request.clone(), wrong_namespace)
            .expect_err("namespace drift");
        assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
        let wrong_family = broker
            .issue_storage_namespace_capability(broker.seal_storage_namespace_proof(
                ManagedStorageCapabilityFamilyV1::AtomicObject,
                CanonicalHash::from_bytes([3u8; 32]),
            ))
            .expect("wrong family capability");
        let error = service
            .admit_namespace(request, wrong_family)
            .expect_err("family drift");
        assert!(matches!(error, ManagedStorageErrorV1::CapabilityMismatch));
    }
}
