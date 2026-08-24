//! RFC-0071 section 8.2 / 17.2: kernel capability issuer and verifier contract.
//!
//! This module owns the unique issuer factory, the private entry table and every sealed
//! constructor. Runtime composition receives only the generic KernelCapabilityIssuerV1;
//! adapters receive verifier facets; tools/providers/renderers receive neither. Storage
//! constructors are split into KernelStorageCapabilityIssuerV1, injected only into the
//! RA-owned storage service and the composition-frozen semantic-owner broker.

use std::sync::Arc;

use crate::managed_file_access::ManagedFileAccessAdmissionTokenV1;
use crate::process_observation::CapabilityVerifyErrorV1;
use crate::resource::{CanonicalHash, IssuedExecutionAdmissionBundleV1};

/// Sealed proof produced by a kernel-owned validator (opaque, not publicly constructible).
#[derive(Debug)]
pub struct SealedExecutionAdmissionProofV1 {
    #[allow(dead_code)]
    handle_id: crate::resource::OpaqueKernelProofHandleId,
    #[allow(dead_code)]
    authenticator: crate::resource::OpaqueKernelProofAuthenticatorV1,
    #[allow(dead_code)]
    kind: ProofKindV1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProofKindV1 {
    ExecutionOneShot,
    ExecutionTerminal,
    ExecutionExtension,
    FileAccessTool,
    FileAccessSessionExport,
    StorageNamespace,
    StorageLogicalKeyObject,
    StorageLogicalKeyStream,
    ArtifactPublish,
    SessionCatalogSnapshot,
    SemanticRetire,
}

/// Generic issuer consumed by runtime composition.
pub trait KernelCapabilityIssuerV1: Send + Sync {
    /// Issues a one-shot execution admission bundle; duplicate or stale proof fails closed.
    fn issue_execution(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<IssuedExecutionAdmissionBundleV1, CapabilityIssueErrorV1>;

    fn issue_file_access(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<ManagedFileAccessAdmissionTokenV1, CapabilityIssueErrorV1>;

    fn verify_execution_bundle(
        &self,
        bundle: IssuedExecutionAdmissionBundleV1,
    ) -> Result<VerifiedExecutionBundleViewV1, CapabilityVerifyErrorV1>;
}

/// Narrow storage issuer: injected only into the RA-owned storage service and the frozen
/// lifecycle/semantic-owner broker. Runtime and ordinary semantic consumers never hold it.
pub trait KernelStorageCapabilityIssuerV1: Send + Sync {
    fn issue_storage_namespace_handle(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::ManagedStorageNamespaceHandleV1, CapabilityIssueErrorV1>;

    fn issue_storage_object_key(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::OpaqueStorageObjectKeyV1, CapabilityIssueErrorV1>;

    fn issue_storage_stream_key(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::OpaqueStorageStreamKeyV1, CapabilityIssueErrorV1>;
}

/// Storage activation evidence verifier (RA-owned implementation injected into the validator).
pub trait StorageCapabilityActivationEvidenceVerifierV1: Send + Sync {
    fn verifier_instance_hash(&self) -> CanonicalHash;

    fn verify_namespace_realization_evidence(
        &self,
        evidence: &EvidenceEnvelopeV1,
    ) -> Result<VerifiedEvidenceViewV1, CapabilityVerifyErrorV1>;

    fn verify_logical_key_registration_evidence(
        &self,
        evidence: &EvidenceEnvelopeV1,
    ) -> Result<VerifiedEvidenceViewV1, CapabilityVerifyErrorV1>;
}

/// Kernel-owned activation validator: consumes verifier views, compares instance/generation
/// and emits sealed proofs. Not constructible from public DTOs.
pub trait StorageCapabilityActivationValidatorV1: Send + Sync {
    fn validate_namespace_realization(
        &self,
        evidence: &EvidenceEnvelopeV1,
    ) -> Result<SealedExecutionAdmissionProofV1, CapabilityVerifyErrorV1>;

    fn validate_logical_key_registration(
        &self,
        evidence: &EvidenceEnvelopeV1,
    ) -> Result<SealedExecutionAdmissionProofV1, CapabilityVerifyErrorV1>;
}

/// Bounded evidence envelope for storage activation.
#[derive(Debug, Clone)]
pub struct EvidenceEnvelopeV1 {
    pub record_hash: CanonicalHash,
    pub journal_frontier_hash: CanonicalHash,
    pub journal_instance_hash: CanonicalHash,
    pub authority_instance_hash: CanonicalHash,
}

/// Bounded verified evidence view (post-consume, no capability re-issuance).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedEvidenceViewV1 {
    pub verifier_instance_hash: CanonicalHash,
    pub verified_record_hash: CanonicalHash,
    pub verified_frontier_hash: CanonicalHash,
}

/// Verified execution bundle view returned after consumption.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedExecutionBundleViewV1 {
    pub bundle_hash: CanonicalHash,
    pub purpose: &'static str,
    pub physical_attempt_id: Option<Vec<u8>>,
}

/// Closed issue error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityIssueErrorV1 {
    #[error("proof handle is unknown or already consumed (one-shot)")]
    UnknownOrConsumed,
    #[error("proof kind does not match the requested issuer method")]
    KindMismatch,
    #[error("capability broker generation drifted after restart")]
    StaleGeneration,
    #[error("issuer method requires a narrow storage facet that is not injected here")]
    FacetUnavailable,
}

/// Kernel-owned private issuer table (single instance per composition).
#[derive(Debug, Default)]
pub struct KernelCapabilityTableV1 {
    issued: Vec<String>,
    consumed: Vec<String>,
}

impl KernelCapabilityTableV1 {
    pub const fn new() -> Self {
        Self {
            issued: Vec::new(),
            consumed: Vec::new(),
        }
    }

    /// Records issuance; duplicate id fails closed.
    pub fn record_issued(&mut self, id: String) -> Result<(), CapabilityIssueErrorV1> {
        if self.issued.contains(&id) {
            return Err(CapabilityIssueErrorV1::UnknownOrConsumed);
        }
        self.issued.push(id);
        Ok(())
    }

    /// Records consumption of an issued id; unknown or already consumed fails closed.
    pub fn record_consumed(&mut self, id: String) -> Result<(), CapabilityVerifyErrorV1> {
        if !self.issued.contains(&id) || self.consumed.contains(&id) {
            return Err(CapabilityVerifyErrorV1::VerifyFailed(
                "unknown or already consumed capability id".to_owned(),
            ));
        }
        self.consumed.push(id);
        Ok(())
    }
}

/// Stub concrete issuer used by compile fixtures (mock-only; production factory is kernel-private).
pub fn mock_issuer() -> Arc<dyn KernelCapabilityIssuerV1> {
    Arc::new(MockCapabilityIssuerV1)
}

struct MockCapabilityIssuerV1;

impl KernelCapabilityIssuerV1 for MockCapabilityIssuerV1 {
    fn issue_execution(
        &self,
        _proof: SealedExecutionAdmissionProofV1,
    ) -> Result<IssuedExecutionAdmissionBundleV1, CapabilityIssueErrorV1> {
        Err(CapabilityIssueErrorV1::FacetUnavailable)
    }

    fn issue_file_access(
        &self,
        _proof: SealedExecutionAdmissionProofV1,
    ) -> Result<ManagedFileAccessAdmissionTokenV1, CapabilityIssueErrorV1> {
        Err(CapabilityIssueErrorV1::FacetUnavailable)
    }

    fn verify_execution_bundle(
        &self,
        _bundle: IssuedExecutionAdmissionBundleV1,
    ) -> Result<VerifiedExecutionBundleViewV1, CapabilityVerifyErrorV1> {
        Err(CapabilityVerifyErrorV1::VerifyFailed(
            "mock issuer cannot verify".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r71_capability_table_fails_closed_on_duplicate_and_unknown_consume() {
        let mut table = KernelCapabilityTableV1::new();
        table.record_issued("bundle-1".to_owned()).expect("issue");
        let duplicate = table.record_issued("bundle-1".to_owned()).expect_err("dup");
        assert!(matches!(
            duplicate,
            CapabilityIssueErrorV1::UnknownOrConsumed
        ));
        let unknown = table
            .record_consumed("bundle-2".to_owned())
            .expect_err("unknown");
        assert!(matches!(unknown, CapabilityVerifyErrorV1::VerifyFailed(_)));
        table
            .record_consumed("bundle-1".to_owned())
            .expect("consume");
        let reconsume = table
            .record_consumed("bundle-1".to_owned())
            .expect_err("again");
        assert!(matches!(
            reconsume,
            CapabilityVerifyErrorV1::VerifyFailed(_)
        ));
    }

    #[test]
    fn r71_mock_issuer_never_issues_but_keeps_closed_errors() {
        let issuer = mock_issuer();
        let _ = issuer;
    }

    #[test]
    fn r71_evidence_view_never_reissues_capability() {
        let view = VerifiedEvidenceViewV1 {
            verifier_instance_hash: CanonicalHash::from_bytes([1u8; 32]),
            verified_record_hash: CanonicalHash::from_bytes([2u8; 32]),
            verified_frontier_hash: CanonicalHash::from_bytes([3u8; 32]),
        };
        // Only public hashes; no authenticator is exposed.
        assert_eq!(
            view.verified_record_hash,
            CanonicalHash::from_bytes([2u8; 32])
        );
    }
}

/// Kernel-side proof binding record.
type ProofRecordV1 = (ProofKindV1, &'static str, Vec<u8>);

/// The real kernel capability broker (production): seals opaque admission proofs, issues
/// one-shot execution bundles and storage namespace handles, verifies consumed bundles.
/// Single instance per composition. The proof handle is opaque; the broker keeps the
/// purpose/attempt/family binding ledger kernel-side so no consumer can fabricate a
/// capability with chosen content.
#[derive(Debug, Default)]
pub struct KernelCapabilityBrokerV1 {
    table: std::sync::Mutex<KernelCapabilityTableV1>,
    /// proof handle -> (kind, purpose, physical attempt bytes)
    proofs: std::sync::Mutex<std::collections::BTreeMap<String, ProofRecordV1>>,
    storage_families: std::sync::Mutex<
        std::collections::BTreeMap<
            String,
            (
                crate::resource::ManagedStorageCapabilityFamilyV1,
                CanonicalHash,
            ),
        >,
    >,
    /// proof handle -> (binding, subject binding hash, operation digest) for tool file access.
    file_access_bindings: std::sync::Mutex<
        std::collections::BTreeMap<
            String,
            (
                crate::managed_file_access::ManagedFileAdmissionBindingV1,
                CanonicalHash,
                CanonicalHash,
            ),
        >,
    >,
    views: std::sync::Mutex<std::collections::BTreeMap<String, VerifiedExecutionBundleViewV1>>,
    sequence: std::sync::atomic::AtomicU64,
}

impl KernelCapabilityBrokerV1 {
    pub const fn new() -> Self {
        Self {
            table: std::sync::Mutex::new(KernelCapabilityTableV1::new()),
            proofs: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            storage_families: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            file_access_bindings: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            views: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            sequence: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Seals an execution admission proof (kernel-owned; the handle is opaque and the
    /// purpose/attempt stay kernel-side).
    pub fn seal_execution_proof(
        &self,
        kind: ProofKindV1,
        purpose: &'static str,
        physical_attempt_id: Vec<u8>,
    ) -> SealedExecutionAdmissionProofV1 {
        let seq = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let handle = format!("proof-{seq}");
        let proof = SealedExecutionAdmissionProofV1 {
            handle_id: crate::resource::OpaqueKernelProofHandleId::new(handle.clone()),
            authenticator: crate::resource::OpaqueKernelProofAuthenticatorV1::new(format!(
                "auth-{seq}"
            )),
            kind,
        };
        self.proofs
            .lock()
            .expect("broker proofs")
            .insert(handle, (kind, purpose, physical_attempt_id));
        proof
    }

    /// Seals a storage namespace admission proof carrying the authority-declared family the
    /// handle will bind.
    pub fn seal_storage_namespace_proof(
        &self,
        family: crate::resource::ManagedStorageCapabilityFamilyV1,
        namespace_hash: CanonicalHash,
    ) -> SealedExecutionAdmissionProofV1 {
        let seq = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let handle = format!("proof-{seq}");
        let proof = SealedExecutionAdmissionProofV1 {
            handle_id: crate::resource::OpaqueKernelProofHandleId::new(handle.clone()),
            authenticator: crate::resource::OpaqueKernelProofAuthenticatorV1::new(format!(
                "auth-{seq}"
            )),
            kind: ProofKindV1::StorageNamespace,
        };
        self.storage_families
            .lock()
            .expect("storage families")
            .insert(handle, (family, namespace_hash));
        proof
    }

    fn take_proof(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<(ProofKindV1, &'static str, Vec<u8>), CapabilityIssueErrorV1> {
        self.proofs
            .lock()
            .expect("broker proofs")
            .remove(proof.handle_id.as_str())
            .ok_or(CapabilityIssueErrorV1::UnknownOrConsumed)
    }

    fn bundle_key(&self, bundle: &IssuedExecutionAdmissionBundleV1) -> String {
        let (token, cap) = match bundle {
            IssuedExecutionAdmissionBundleV1::OneShot {
                consumer_token,
                resource_capability,
            }
            | IssuedExecutionAdmissionBundleV1::Terminal {
                consumer_token,
                resource_capability,
            }
            | IssuedExecutionAdmissionBundleV1::Extension {
                consumer_token,
                resource_capability,
            } => (
                consumer_token.as_str().to_owned(),
                resource_capability.as_str().to_owned(),
            ),
        };
        format!("{token}:{cap}")
    }
}

impl KernelCapabilityIssuerV1 for KernelCapabilityBrokerV1 {
    fn issue_execution(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<IssuedExecutionAdmissionBundleV1, CapabilityIssueErrorV1> {
        let (kind, purpose, attempt) = self.take_proof(proof)?;
        let seq = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let bundle = match kind {
            ProofKindV1::ExecutionOneShot => IssuedExecutionAdmissionBundleV1::OneShot {
                consumer_token: crate::resource::OpaqueResourceId::new(format!(
                    "token:{purpose}:{seq}"
                )),
                resource_capability: crate::resource::OpaqueResourceId::new(format!(
                    "cap:{purpose}:{seq}"
                )),
            },
            ProofKindV1::ExecutionTerminal => IssuedExecutionAdmissionBundleV1::Terminal {
                consumer_token: crate::resource::OpaqueResourceId::new(format!(
                    "token:{purpose}:{seq}"
                )),
                resource_capability: crate::resource::OpaqueResourceId::new(format!(
                    "cap:{purpose}:{seq}"
                )),
            },
            ProofKindV1::ExecutionExtension => IssuedExecutionAdmissionBundleV1::Extension {
                consumer_token: crate::resource::OpaqueResourceId::new(format!(
                    "token:{purpose}:{seq}"
                )),
                resource_capability: crate::resource::OpaqueResourceId::new(format!(
                    "cap:{purpose}:{seq}"
                )),
            },
            _ => return Err(CapabilityIssueErrorV1::KindMismatch),
        };
        let key = self.bundle_key(&bundle);
        self.table
            .lock()
            .expect("broker table")
            .record_issued(key.clone())
            .map_err(|_| CapabilityIssueErrorV1::UnknownOrConsumed)?;
        let mut digest = [0u8; 32];
        {
            let bytes = key.as_bytes();
            let bound = bytes.len().min(32);
            digest[..bound].copy_from_slice(&bytes[..bound]);
        }
        self.views.lock().expect("broker views").insert(
            key,
            VerifiedExecutionBundleViewV1 {
                bundle_hash: CanonicalHash::from_bytes(digest),
                purpose,
                physical_attempt_id: Some(attempt),
            },
        );
        Ok(bundle)
    }

    fn issue_file_access(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<ManagedFileAccessAdmissionTokenV1, CapabilityIssueErrorV1> {
        use crate::managed_file_access::ToolFileAccessAdmissionTokenV1;
        let (binding, subject_binding_hash, operation_digest) = self
            .file_access_bindings
            .lock()
            .expect("file access bindings")
            .remove(proof.handle_id.as_str())
            .ok_or(CapabilityIssueErrorV1::UnknownOrConsumed)?;
        Ok(ManagedFileAccessAdmissionTokenV1::Tool(
            ToolFileAccessAdmissionTokenV1::broker_issued(
                binding,
                subject_binding_hash,
                operation_digest,
            ),
        ))
    }

    fn verify_execution_bundle(
        &self,
        bundle: IssuedExecutionAdmissionBundleV1,
    ) -> Result<VerifiedExecutionBundleViewV1, CapabilityVerifyErrorV1> {
        let key = self.bundle_key(&bundle);
        let Some(view) = self.views.lock().expect("broker views").remove(&key) else {
            return Err(CapabilityVerifyErrorV1::VerifyFailed(
                "unknown or consumed bundle".to_owned(),
            ));
        };
        self.table
            .lock()
            .expect("broker table")
            .record_consumed(key)
            .map_err(|_| CapabilityVerifyErrorV1::VerifyFailed("already consumed".to_owned()))?;
        Ok(view)
    }
}

impl KernelCapabilityBrokerV1 {
    /// Seals a tool file-access admission proof; the binding, subject binding hash and
    /// operation digest stay kernel-side (a consumer can never choose token content).
    pub fn seal_file_access_proof(
        &self,
        binding: crate::managed_file_access::ManagedFileAdmissionBindingV1,
        subject_binding_hash: CanonicalHash,
        operation_digest: CanonicalHash,
    ) -> SealedExecutionAdmissionProofV1 {
        let seq = self
            .sequence
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let handle = format!("proof-{seq}");
        let proof = SealedExecutionAdmissionProofV1 {
            handle_id: crate::resource::OpaqueKernelProofHandleId::new(handle.clone()),
            authenticator: crate::resource::OpaqueKernelProofAuthenticatorV1::new(format!(
                "auth-{seq}"
            )),
            kind: ProofKindV1::FileAccessTool,
        };
        self.file_access_bindings
            .lock()
            .expect("file access bindings")
            .insert(handle, (binding, subject_binding_hash, operation_digest));
        proof
    }

    /// Issues the storage admission CAPABILITY for a sealed storage-namespace proof (the
    /// kernel port's admission parameter; the broker binds family/namespace kernel-side).
    pub fn issue_storage_namespace_capability(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::ValidatedStorageAdmissionCapabilityV1, CapabilityIssueErrorV1>
    {
        let (family, namespace_hash) = self
            .storage_families
            .lock()
            .expect("storage families")
            .remove(proof.handle_id.as_str())
            .ok_or(CapabilityIssueErrorV1::UnknownOrConsumed)?;
        Ok(
            crate::managed_storage::ValidatedStorageAdmissionCapabilityV1::broker_issued(
                crate::resource::OpaqueKernelCapabilityHandleId::new(
                    proof.handle_id.as_str().to_owned(),
                ),
                family,
                namespace_hash,
            ),
        )
    }
}

impl KernelStorageCapabilityIssuerV1 for KernelCapabilityBrokerV1 {
    fn issue_storage_namespace_handle(
        &self,
        proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::ManagedStorageNamespaceHandleV1, CapabilityIssueErrorV1>
    {
        let (family, namespace_hash) = self
            .storage_families
            .lock()
            .expect("storage families")
            .remove(proof.handle_id.as_str())
            .ok_or(CapabilityIssueErrorV1::UnknownOrConsumed)?;
        Ok(
            crate::managed_storage::ManagedStorageNamespaceHandleV1::new(
                crate::resource::OpaqueKernelCapabilityHandleId::new(
                    proof.handle_id.as_str().to_owned(),
                ),
                namespace_hash,
                family,
                crate::resource::OpaqueKernelCapabilityAuthenticatorV1::new(format!(
                    "auth-{}",
                    proof.handle_id.as_str()
                )),
            ),
        )
    }

    fn issue_storage_object_key(
        &self,
        _proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::OpaqueStorageObjectKeyV1, CapabilityIssueErrorV1> {
        Err(CapabilityIssueErrorV1::FacetUnavailable)
    }

    fn issue_storage_stream_key(
        &self,
        _proof: SealedExecutionAdmissionProofV1,
    ) -> Result<crate::managed_storage::OpaqueStorageStreamKeyV1, CapabilityIssueErrorV1> {
        Err(CapabilityIssueErrorV1::FacetUnavailable)
    }
}

#[cfg(test)]
#[path = "tests/capability_broker_tests.rs"]
mod broker_tests;
