use super::*;
use sigil_kernel::managed_storage::StorageAdmissionSourceV1;

fn zero_hash() -> sigil_kernel::resource::CanonicalHash {
    sigil_kernel::resource::CanonicalHash::from_bytes([0u8; 32])
}
use sigil_kernel::resource::OpaqueWorkspaceId;

fn request(
    owner: ManagedStorageSemanticOwnerV1,
    family: ManagedStorageCapabilityFamilyV1,
    source: StorageAdmissionSourceClassV1,
    purpose: ManagedStorageAdmissionPurposeV1,
) -> ManagedStorageAdmissionRequestV1 {
    let source_value = match source {
        StorageAdmissionSourceClassV1::ApplicationLifecycleReady => {
            StorageAdmissionSourceV1::ApplicationLifecycleReady {
                cutover_manifest_hash: zero_hash(),
                application_generation: 1,
                control_grant_hash: zero_hash(),
                control_frontier_hash: zero_hash(),
                lifecycle_grant_hash: zero_hash(),
                lifecycle_admission_frontier_hash: zero_hash(),
            }
        }
        StorageAdmissionSourceClassV1::SessionLifecycle => {
            StorageAdmissionSourceV1::SessionLifecycle {
                session_scope: "s1".to_owned(),
                session_generation: 1,
                workspace_scope: "w1".to_owned(),
                lifecycle_event_digest: zero_hash(),
                lifecycle_log_grant_hash: zero_hash(),
                lifecycle_frontier_hash: zero_hash(),
            }
        }
        StorageAdmissionSourceClassV1::WorkspaceLifecycle => {
            StorageAdmissionSourceV1::WorkspaceLifecycle {
                workspace_scope: "w1".to_owned(),
                workspace_generation: 1,
                lifecycle_event_digest: zero_hash(),
                lifecycle_log_grant_hash: zero_hash(),
                lifecycle_frontier_hash: zero_hash(),
            }
        }
        StorageAdmissionSourceClassV1::ApplicationControlReady => {
            StorageAdmissionSourceV1::ApplicationControlReady {
                cutover_manifest_hash: zero_hash(),
                application_generation: 1,
                control_grant_hash: zero_hash(),
                control_admission_frontier_hash: zero_hash(),
            }
        }
        StorageAdmissionSourceClassV1::ToolDecisionInProcessStorage => {
            StorageAdmissionSourceV1::ToolDecisionInProcessStorage {
                storage_plan_hash: zero_hash(),
                requirement_set_hash: zero_hash(),
                operation_digest: zero_hash(),
                decision_hash: zero_hash(),
            }
        }
        StorageAdmissionSourceClassV1::ToolDecisionExecution => {
            StorageAdmissionSourceV1::ToolDecisionExecution {
                permission_plan_hash: zero_hash(),
                decision_hash: zero_hash(),
                execution_draft_hash: zero_hash(),
            }
        }
        StorageAdmissionSourceClassV1::SemanticTransaction => {
            StorageAdmissionSourceV1::SemanticTransaction {
                transaction_id: "t1".to_owned(),
                transaction_hash: zero_hash(),
            }
        }
        _ => StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash: zero_hash(),
            application_generation: 1,
        },
    };
    ManagedStorageAdmissionRequestV1 {
        semantic_owner: owner,
        capability_family: family,
        purpose,
        source: source_value,
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Workspace(
            OpaqueWorkspaceId::new("w1".to_owned()),
        ),
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Workspace(
            OpaqueWorkspaceId::new("w1".to_owned()),
        ),
    }
}

#[test]
fn r71_semantic_matrix_accepts_frozen_cell() {
    use ManagedStorageAdmissionPurposeV1::*;
    use ManagedStorageCapabilityFamilyV1::*;
    let approved = request(
        ManagedStorageSemanticOwnerV1::SessionLog,
        AppendLog,
        StorageAdmissionSourceClassV1::ApplicationLifecycleReady,
        DurablePayload,
    );
    validate_matrix_admission(&approved).expect("approved cell");
}

#[test]
fn r71_semantic_matrix_rejects_cross_swap() {
    use ManagedStorageAdmissionPurposeV1::*;
    use ManagedStorageCapabilityFamilyV1::*;
    // SessionLog with RebuildableDatabaseProjection is not frozen.
    let cross = request(
        ManagedStorageSemanticOwnerV1::SessionLog,
        RebuildableDatabaseProjection,
        StorageAdmissionSourceClassV1::ApplicationLifecycleReady,
        RebuildableProjection,
    );
    let error = validate_matrix_admission(&cross).expect_err("cross swap must fail");
    assert!(matches!(error, MatrixErrorV1::UnknownCombination { .. }));
}

#[test]
fn r71_semantic_matrix_rejects_source_swap() {
    use ManagedStorageAdmissionPurposeV1::*;
    use ManagedStorageCapabilityFamilyV1::*;
    let swapped = request(
        ManagedStorageSemanticOwnerV1::SessionLog,
        AppendLog,
        StorageAdmissionSourceClassV1::SessionLifecycle,
        DurablePayload,
    );
    let error = validate_matrix_admission(&swapped).expect_err("source swap");
    assert!(matches!(error, MatrixErrorV1::UnknownCombination { .. }));
}
