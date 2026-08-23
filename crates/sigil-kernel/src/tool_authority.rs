//! RFC-0071 section 8.5 / R71.6: kernel-owned tool authority facade.
//!
//! One kernel-side entry for in-process file tools: seal a tool file-access admission proof
//! (binding kernel-side), issue the one-shot token and adjudicate through the authority's
//! pathless port. The tool never fabricates a token nor chooses binding content; the facade
//! composes the broker and the adjudicator exactly once per application.

use std::sync::Arc;

use crate::capability_issuer::{CapabilityIssueErrorV1, KernelCapabilityBrokerV1};
use crate::managed_file_access::{
    ManagedFileAccessErrorV1, ManagedFileAccessRequestV1, ManagedFileAccessResultV1,
    ManagedFileAccessServiceV1, ManagedFileAdmissionBindingV1, ManagedFileOperationV1,
};
use crate::resource::{CanonicalHash, OpaquePermissionSubjectRef};

/// Closed tool-authority error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KernelToolAuthorityErrorV1 {
    #[error("tool admission binding is not a ToolPermissionPlan: {0}")]
    BindingKind(String),
    #[error("capability issuance failed: {0:?}")]
    Issuance(CapabilityIssueErrorV1),
    #[error("file access adjudication failed: {0}")]
    Access(ManagedFileAccessErrorV1),
}

/// Kernel-owned tool authority facade (broker + adjudicator, one per application).
pub struct KernelToolAuthorityV1 {
    file_access: Arc<dyn ManagedFileAccessServiceV1>,
    broker: Arc<KernelCapabilityBrokerV1>,
}

impl KernelToolAuthorityV1 {
    pub fn new(
        file_access: Arc<dyn ManagedFileAccessServiceV1>,
        broker: Arc<KernelCapabilityBrokerV1>,
    ) -> Self {
        Self {
            file_access,
            broker,
        }
    }

    /// Adjudicates one in-process file-tool operation: seal -> issue (one-shot) -> port access.
    /// The returned receipt is the durable borrowed-access fact for that operation.
    pub fn adjudicate_tool_file_access(
        &self,
        binding: ManagedFileAdmissionBindingV1,
        subject_ref: &OpaquePermissionSubjectRef,
        operation: ManagedFileOperationV1,
    ) -> Result<ManagedFileAccessResultV1, KernelToolAuthorityErrorV1> {
        let (subject_binding_hash, admission_hash) = match &binding {
            ManagedFileAdmissionBindingV1::ToolPermissionPlan {
                file_subject_binding_hash,
                file_access_plan_hash,
                ..
            } => (*file_subject_binding_hash, *file_access_plan_hash),
            other => {
                return Err(KernelToolAuthorityErrorV1::BindingKind(format!(
                    "{other:?}"
                )));
            }
        };
        let operation_digest = operation_digest_for(operation, subject_binding_hash);
        let proof = self.broker.seal_file_access_proof(
            binding.clone(),
            subject_binding_hash,
            operation_digest,
        );
        let token = crate::capability_issuer::KernelCapabilityIssuerV1::issue_file_access(
            self.broker.as_ref(),
            proof,
        )
        .map_err(KernelToolAuthorityErrorV1::Issuance)?;
        self.file_access
            .access(
                ManagedFileAccessRequestV1 {
                    subject_ref: subject_ref.clone(),
                    operation,
                    operation_digest,
                    admission_binding: binding,
                    admission_binding_hash: admission_hash,
                },
                token,
            )
            .map_err(KernelToolAuthorityErrorV1::Access)
    }
}

/// Stable operation digest: closed tag plus subject binding (never a raw SQL/path string).
fn operation_digest_for(
    operation: ManagedFileOperationV1,
    subject_binding_hash: CanonicalHash,
) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(match operation {
        ManagedFileOperationV1::Read => b"read".as_slice(),
        ManagedFileOperationV1::List => b"list".as_slice(),
        ManagedFileOperationV1::Glob => b"glob".as_slice(),
        ManagedFileOperationV1::Grep => b"grep".as_slice(),
        ManagedFileOperationV1::Write => b"write".as_slice(),
        ManagedFileOperationV1::Edit => b"edit".as_slice(),
        ManagedFileOperationV1::Delete => b"delete".as_slice(),
        ManagedFileOperationV1::Rename => b"rename".as_slice(),
    });
    hasher.update(subject_binding_hash.as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

#[cfg(test)]
#[path = "tests/tool_authority_tests.rs"]
mod tests;
