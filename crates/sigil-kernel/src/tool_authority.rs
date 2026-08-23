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
#[derive(Clone)]
pub struct KernelToolAuthorityV1 {
    file_access: Arc<dyn ManagedFileAccessServiceV1>,
    broker: Arc<KernelCapabilityBrokerV1>,
}

impl std::fmt::Debug for KernelToolAuthorityV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KernelToolAuthorityV1")
            .field("adjudicator", &"attached")
            .field("broker", &"attached")
            .finish()
    }
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

/// Builds the ToolPermissionPlan admission binding from the V3 file-access plan ref plus the
/// decision/continuity digests the runtime holds (pure mapping; no I/O).
pub fn v3_file_access_binding(
    permission_plan_hash: CanonicalHash,
    decision_hash: CanonicalHash,
    approval_continuity_hash: CanonicalHash,
    tool_start_event_digest: CanonicalHash,
    file_ref: &crate::permission_plan_v3::ManagedFileAccessPlanDraftRefV1,
) -> ManagedFileAdmissionBindingV1 {
    ManagedFileAdmissionBindingV1::ToolPermissionPlan {
        permission_plan_hash,
        decision_hash,
        approval_continuity_hash,
        tool_start_event_digest,
        file_access_plan_hash: file_ref.plan_hash,
        file_subject_binding_hash: file_ref.subject_binding_hash,
        file_resolver_proof_digest: file_ref.resolver_proof_digest,
        file_authority_generation: file_ref.authority_generation,
        workspace_mutation_activation: None,
    }
}

/// Guards one tool file operation through the context's attached authority: None when no
/// authority is attached (legacy path), Err on any refusal (fail closed), the receipt
/// otherwise. Tools call this before any filesystem access when the subject is borrowed.
pub fn adjudicate_guarded_tool_operation(
    tool_authority: Option<&KernelToolAuthorityV1>,
    binding: &ManagedFileAdmissionBindingV1,
    subject_ref: &OpaquePermissionSubjectRef,
    operation: ManagedFileOperationV1,
) -> Result<Option<ManagedFileAccessResultV1>, KernelToolAuthorityErrorV1> {
    let Some(authority) = tool_authority else {
        return Ok(None);
    };
    authority
        .adjudicate_tool_file_access(binding.clone(), subject_ref, operation)
        .map(Some)
}

/// Builds the ToolPermissionPlan admission binding for a tool operation when the context
/// carries the sealed V3 plan and decision (integrity: decision must bind the same plan hash;
/// a mismatch fails closed). None when no V3 plan is attached (legacy V2 path).
pub fn adjudicate_v3_file_operation(
    v3_plan: Option<&crate::permission_plan_v3::ToolPermissionPlanV3>,
    v3_decision: Option<&crate::permission_plan_v3::ToolPermissionDecisionV3>,
    tool_authority: Option<&KernelToolAuthorityV1>,
    operation: ManagedFileOperationV1,
) -> Result<Option<ManagedFileAccessResultV1>, KernelToolAuthorityErrorV1> {
    let Some(plan) = v3_plan else {
        return Ok(None);
    };
    let Some(file_ref) = plan.managed_file_access_plan.as_ref() else {
        return Err(KernelToolAuthorityErrorV1::BindingKind(
            "tool declares no managed file access plan".to_owned(),
        ));
    };
    // Decision integrity: the approved decision must bind the exact sealed plan; a drift here
    // means the tool call was approved against a different plan and must be refused.
    if let Some(decision) = v3_decision
        && decision.plan_hash != plan.plan_hash
    {
        return Err(KernelToolAuthorityErrorV1::BindingKind(
            "decision binds a different plan hash".to_owned(),
        ));
    }
    let binding = v3_file_access_binding(
        plan.plan_hash,
        v3_decision
            .map(|decision| decision.decision_hash)
            .unwrap_or(CanonicalHash::from_bytes([0u8; 32])),
        CanonicalHash::from_bytes([0u8; 32]),
        CanonicalHash::from_bytes([0u8; 32]),
        file_ref,
    );
    adjudicate_guarded_tool_operation(tool_authority, &binding, &file_ref.subject_ref, operation)
}

#[cfg(test)]
#[path = "tests/tool_authority_mapping_tests.rs"]
mod mapping_tests;
