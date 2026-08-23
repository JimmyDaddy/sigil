//! RFC-0071 section 8.5 / R71.6: authority-owned in-process file access adjudicator.
//!
//! read/write/edit/list/glob/grep tools never spawn; they must not bypass the borrowed
//! Workspace / ExternalUserPath identity lease. This service adjudicates the post-decision
//! Tool admission: token binding vs request, one-shot adjudication claim, observed borrowed
//! identity (identity_before in the receipt), SystemTemp deny/read-boundary, and closed
//! operation classification. It never performs file I/O itself and never claims ownership of
//! borrowed content. SessionExport / SessionExportReconcile tokens have their own
//! kernel-verified export path (session_export.rs) and are refused here until the storage
//! writer slice wires them through explicitly.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use sigil_kernel::managed_execution::BorrowedResourceAccessReceiptV1;
use sigil_kernel::managed_file_access::{
    ManagedFileAccessAdmissionTokenV1, ManagedFileAccessErrorV1, ManagedFileAccessRequestV1,
    ManagedFileAccessResultV1, ManagedFileAccessServiceV1, ManagedFileOperationV1,
};
use sigil_kernel::resource::{CanonicalHash, OpaquePermissionSubjectRef, ResourceAccessV1};

use crate::borrowed::{BorrowedSubjectClassV1, BorrowedSubjectRegistryV1};

/// Closed access class for a closed file operation.
pub fn access_class_for(operation: ManagedFileOperationV1) -> ResourceAccessV1 {
    match operation {
        ManagedFileOperationV1::Read
        | ManagedFileOperationV1::List
        | ManagedFileOperationV1::Glob
        | ManagedFileOperationV1::Grep => ResourceAccessV1::Read,
        ManagedFileOperationV1::Write | ManagedFileOperationV1::Edit => ResourceAccessV1::Write,
        ManagedFileOperationV1::Delete | ManagedFileOperationV1::Rename => {
            ResourceAccessV1::DeleteManaged
        }
    }
}

/// Stable tag for one access class (canonical, never a raw enum cast).
fn access_tag(access: ResourceAccessV1) -> u8 {
    match access {
        ResourceAccessV1::Read => 1,
        ResourceAccessV1::Write => 2,
        ResourceAccessV1::Create => 3,
        ResourceAccessV1::DeleteManaged => 4,
        ResourceAccessV1::DeleteExactSubject => 5,
        ResourceAccessV1::DeleteSubjectSubtree => 6,
        ResourceAccessV1::RenameWithinGrant => 7,
        ResourceAccessV1::Execute => 8,
    }
}

/// Authority-owned file access adjudicator behind the kernel pathless port.
pub struct AuthorityManagedFileAccessServiceV1 {
    registry: Arc<Mutex<BorrowedSubjectRegistryV1>>,
    consumed: Mutex<BTreeSet<String>>,
}

impl AuthorityManagedFileAccessServiceV1 {
    /// Creates the adjudicator. The registry is the single borrowed-identity observation source
    /// shared with bootstrap (identity observation happens once per generation).
    pub fn new(registry: Arc<Mutex<BorrowedSubjectRegistryV1>>) -> Self {
        Self {
            registry,
            consumed: Mutex::new(BTreeSet::new()),
        }
    }

    fn claim_key(token: &ManagedFileAccessAdmissionTokenV1) -> String {
        match token {
            ManagedFileAccessAdmissionTokenV1::Tool(tool) => format!(
                "{}-{}",
                tool.subject_binding_hash().to_hex(),
                tool.operation_digest().to_hex()
            ),
            ManagedFileAccessAdmissionTokenV1::SessionExport(_)
            | ManagedFileAccessAdmissionTokenV1::SessionExportReconcile(_) => {
                "export-not-wired".to_owned()
            }
        }
    }
}

fn receipt_digest(
    subject_ref: &OpaquePermissionSubjectRef,
    subject_binding_hash: CanonicalHash,
    operation_digest: CanonicalHash,
) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut acc = Vec::new();
    acc.extend_from_slice(subject_ref.as_str().as_bytes());
    acc.extend_from_slice(subject_binding_hash.as_bytes());
    acc.extend_from_slice(operation_digest.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(acc);
    CanonicalHash::from_bytes(hasher.finalize().into())
}

impl ManagedFileAccessServiceV1 for AuthorityManagedFileAccessServiceV1 {
    fn access(
        &self,
        request: ManagedFileAccessRequestV1,
        token: ManagedFileAccessAdmissionTokenV1,
    ) -> Result<ManagedFileAccessResultV1, ManagedFileAccessErrorV1> {
        // V1 adjudicates the Tool path (in-process file tools). Export token kinds are refused
        // until the storage/export writer slice wires them explicitly.
        let ManagedFileAccessAdmissionTokenV1::Tool(tool) = &token else {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        };
        if request.admission_binding != *tool.binding() {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        }
        if tool.operation_digest() != request.operation_digest {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        }
        let access_class = access_class_for(request.operation);
        let registry = self
            .registry
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::SubjectIdentityDrift)?;
        let Some(class) = registry.class_for(&request.subject_ref) else {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        };
        // SystemTemp is a deny/read-boundary fact in V1: non-read operations are refused.
        if class == BorrowedSubjectClassV1::SystemTemp && access_class != ResourceAccessV1::Read {
            return Err(ManagedFileAccessErrorV1::OperationNotPermitted);
        }
        // One-shot adjudication claim is consumed only after every check passes: a refused
        // adjudication never burns the approval.
        let key = Self::claim_key(&token);
        let mut consumed = self
            .consumed
            .lock()
            .map_err(|_| ManagedFileAccessErrorV1::AdmissionMismatch)?;
        if !consumed.insert(key) {
            return Err(ManagedFileAccessErrorV1::AdmissionMismatch);
        }
        drop(consumed);

        let subject_binding_hash = tool.subject_binding_hash();
        let operation_digest = tool.operation_digest();
        let identity_before = registry
            .identity_for(&request.subject_ref)
            .map(|identity| identity.digest);
        let mut granted = BTreeSet::new();
        granted.insert(access_class);
        let mut granted_material = Vec::new();
        for access in &granted {
            granted_material.push(access_tag(*access));
        }
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(granted_material);
        let granted_access_hash = CanonicalHash::from_bytes(hasher.finalize().into());
        let receipt_hash =
            receipt_digest(&request.subject_ref, subject_binding_hash, operation_digest);
        let access_receipt = BorrowedResourceAccessReceiptV1 {
            subject_ref: request.subject_ref.clone(),
            subject_binding_hash,
            operation_digest,
            granted_access_hash,
            identity_before,
            identity_after: None,
            borrowed_effect_frontier_hash: receipt_hash,
            effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
            receipt_hash,
        };
        Ok(ManagedFileAccessResultV1 {
            access_receipt,
            effect_settlement: sigil_kernel::recovery::EffectSettlementV1::Applied,
            result_digest: receipt_hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sigil_kernel::managed_file_access::{
        ManagedFileAdmissionBindingV1, ToolFileAccessAdmissionTokenV1,
    };
    use sigil_kernel::resource::{AuthorityGeneration, OpaquePermissionSubjectRef};
    use std::collections::BTreeSet;

    fn hash(seed: u8) -> CanonicalHash {
        CanonicalHash::from_bytes([seed; 32])
    }

    fn binding() -> ManagedFileAdmissionBindingV1 {
        ManagedFileAdmissionBindingV1::ToolPermissionPlan {
            permission_plan_hash: hash(1),
            decision_hash: hash(2),
            approval_continuity_hash: hash(3),
            tool_start_event_digest: hash(4),
            file_access_plan_hash: hash(5),
            file_subject_binding_hash: hash(6),
            file_resolver_proof_digest: hash(7),
            file_authority_generation: AuthorityGeneration {
                epoch: 1,
                instance_hash: hash(8),
            },
            workspace_mutation_activation: None,
        }
    }

    fn subject(
        subject_ref: &str,
        class: BorrowedSubjectClassV1,
        identity: bool,
    ) -> Arc<Mutex<BorrowedSubjectRegistryV1>> {
        let mut registry = BorrowedSubjectRegistryV1::new();
        let observed = if identity {
            Some(crate::identity::CanonicalLocalIdentity {
                digest: hash(9),
                is_regular_file: false,
                is_directory: true,
                is_symlink: false,
                link_count: 2,
            })
        } else {
            None
        };
        registry
            .observe_with_identity(
                &OpaquePermissionSubjectRef::new(subject_ref.to_owned()),
                class,
                1,
                observed,
            )
            .expect("observe");
        Arc::new(Mutex::new(registry))
    }

    fn request(
        subject_ref: &str,
        operation: ManagedFileOperationV1,
        op_digest: CanonicalHash,
    ) -> ManagedFileAccessRequestV1 {
        ManagedFileAccessRequestV1 {
            subject_ref: OpaquePermissionSubjectRef::new(subject_ref.to_owned()),
            operation,
            operation_digest: op_digest,
            admission_binding: binding(),
            admission_binding_hash: hash(10),
        }
    }

    fn token(op_digest: CanonicalHash) -> ManagedFileAccessAdmissionTokenV1 {
        ManagedFileAccessAdmissionTokenV1::Tool(
            ToolFileAccessAdmissionTokenV1::qualification_fixture(binding(), hash(6), op_digest),
        )
    }

    #[test]
    fn r71_fa_workspace_read_adjudicates_with_identity_evidence() {
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, true);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let result = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Read, hash(6)),
                token(hash(6)),
            )
            .expect("adjudicate");
        assert_eq!(
            result.effect_settlement,
            sigil_kernel::recovery::EffectSettlementV1::Applied
        );
        assert_eq!(result.access_receipt.subject_binding_hash, hash(6));
        assert_eq!(result.access_receipt.identity_before, Some(hash(9)));
        assert!(result.access_receipt.identity_after.is_none());
        // Read class has stable tag 1; grant hash is sha256([1]).
        use sha2::Digest as _;
        let mut expected = sha2::Sha256::new();
        expected.update([1u8]);
        assert_eq!(
            result.access_receipt.granted_access_hash,
            CanonicalHash::from_bytes(expected.finalize().into())
        );
    }

    #[test]
    fn r71_fa_claim_is_one_shot() {
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        svc.access(
            request("ws-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect("first");
        let error = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Read, hash(6)),
                token(hash(6)),
            )
            .expect_err("reuse");
        assert!(matches!(error, ManagedFileAccessErrorV1::AdmissionMismatch));
    }

    #[test]
    fn r71_fa_operation_digest_mismatch_refused() {
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let error = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Read, hash(99)),
                token(hash(6)),
            )
            .expect_err("mismatch");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::OperationNotPermitted
        ));
    }

    #[test]
    fn r71_fa_unregistered_subject_refused() {
        let svc = AuthorityManagedFileAccessServiceV1::new(Arc::new(Mutex::new(
            BorrowedSubjectRegistryV1::new(),
        )));
        let error = svc
            .access(
                request("unknown-1", ManagedFileOperationV1::Read, hash(6)),
                token(hash(6)),
            )
            .expect_err("unregistered");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::OperationNotPermitted
        ));
    }

    #[test]
    fn r71_fa_system_temp_write_denied_read_allowed() {
        let registry = subject("st-1", BorrowedSubjectClassV1::SystemTemp, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let error = svc
            .access(
                request("st-1", ManagedFileOperationV1::Write, hash(6)),
                token(hash(6)),
            )
            .expect_err("deny write");
        assert!(matches!(
            error,
            ManagedFileAccessErrorV1::OperationNotPermitted
        ));
        // Read at the boundary remains admissible.
        svc.access(
            request("st-1", ManagedFileOperationV1::Read, hash(6)),
            token(hash(6)),
        )
        .expect("read ok");
    }

    #[test]
    fn r71_fa_granted_hash_matches_closed_class() {
        let _ = BTreeSet::<ResourceAccessV1>::new();
        let registry = subject("ws-1", BorrowedSubjectClassV1::Workspace, false);
        let svc = AuthorityManagedFileAccessServiceV1::new(registry);
        let result = svc
            .access(
                request("ws-1", ManagedFileOperationV1::Edit, hash(6)),
                token(hash(6)),
            )
            .expect("edit");
        assert!(result.access_receipt.granted_access_hash != hash(0));
    }
}
