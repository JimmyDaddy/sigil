//! RFC-0071 section 9.5 / R71.1: frozen owner x kind x capability x source x purpose matrix.
//!
//! The matrix is the single closed admission authority for managed storage namespaces. Any
//! unknown combination fails closed before a namespace or primitive lease is granted; golden
//! positive and negative fixtures are generated from this table.

use sigil_kernel::managed_storage::ManagedStorageAdmissionRequestV1;
use sigil_kernel::resource::{
    ManagedStorageAdmissionPurposeV1, ManagedStorageCapabilityFamilyV1,
    ManagedStorageSemanticOwnerV1, ResourceKindV1, StorageAdmissionSourceClassV1,
};

/// Frozen allowed cell: (semantic_owner, capability_family, source_class, purpose).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatrixCellV1 {
    pub owner: &'static str,
    pub family: ManagedStorageCapabilityFamilyV1,
    pub source: StorageAdmissionSourceClassV1,
    pub purpose: ManagedStorageAdmissionPurposeV1,
    pub kind: ResourceKindV1,
}

/// Reads the frozen matrix as a bounded immutable list.
pub fn frozen_matrix() -> &'static [MatrixCellV1] {
    use ManagedStorageAdmissionPurposeV1::*;
    use ManagedStorageCapabilityFamilyV1::*;
    use StorageAdmissionSourceClassV1::*;
    &[
        MatrixCellV1 {
            owner: "SessionLog",
            family: AppendLog,
            source: ApplicationLifecycleReady,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "SessionLifecycleLog",
            family: AppendLog,
            source: SessionLifecycle,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "InteractiveInputHistory",
            family: AtomicObject,
            source: WorkspaceLifecycle,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "DurableMemory",
            family: JournaledAtomicProjection,
            source: ToolDecisionInProcessStorage,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "WorkspaceMutationState",
            family: SemanticLeaseLedger,
            source: ToolDecisionInProcessStorage,
            purpose: SemanticLease,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "ApplicationControlLog",
            family: AppendLog,
            source: ApplicationCutoverRoot,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "PlanStore",
            family: AppendLog,
            source: ApplicationLifecycleReady,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "SessionCatalog",
            family: RebuildableDatabaseProjection,
            source: WorkspaceLifecycle,
            purpose: RebuildableProjection,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "ProviderConnectionState",
            family: AtomicObject,
            source: ApplicationLifecycleReady,
            purpose: DurablePayload,
            kind: ResourceKindV1::RuntimeState,
        },
        MatrixCellV1 {
            owner: "ArtifactStaging",
            family: StreamingArtifact,
            source: ToolDecisionExecution,
            purpose: ArtifactPublish,
            kind: ResourceKindV1::ArtifactStaging,
        },
        MatrixCellV1 {
            owner: "ArtifactStore",
            family: ManagedStorageCapabilityFamilyV1::ArtifactStore,
            source: SemanticTransaction,
            purpose: ArtifactPublish,
            kind: ResourceKindV1::ArtifactStore,
        },
        MatrixCellV1 {
            owner: "RuntimeCache",
            family: AtomicObject,
            source: ApplicationLifecycleReady,
            purpose: CacheRefresh,
            kind: ResourceKindV1::RuntimeCache,
        },
    ]
}

fn cell_matches(cell: &MatrixCellV1, request: &ManagedStorageAdmissionRequestV1) -> bool {
    owner_label(request.semantic_owner) == cell.owner
        && request.capability_family == cell.family
        && request.source.source_class() == cell.source
        && request.purpose == cell.purpose
}

fn kind_matches(kind: ResourceKindV1, request: &ManagedStorageAdmissionRequestV1) -> bool {
    matches!(
        (kind, request.semantic_owner),
        (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::SessionLog
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::SessionLifecycleLog
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::InteractiveInputHistory
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::DurableMemory(_)
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::WorkspaceMutationState
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::ApplicationControlLog
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::PlanStore
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::SessionCatalog
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::ProviderConnectionState
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::AdapterDurableState(_)
        ) | (
            ResourceKindV1::RuntimeState,
            ManagedStorageSemanticOwnerV1::RuntimeCache(_)
        ) | (
            ResourceKindV1::ArtifactStaging,
            ManagedStorageSemanticOwnerV1::ArtifactStaging
        ) | (
            ResourceKindV1::ArtifactStore,
            ManagedStorageSemanticOwnerV1::ArtifactStore
        ) | (
            ResourceKindV1::RuntimeCache,
            ManagedStorageSemanticOwnerV1::RuntimeCache(_)
        )
    )
}

pub fn owner_label(owner: ManagedStorageSemanticOwnerV1) -> &'static str {
    match owner {
        ManagedStorageSemanticOwnerV1::SessionLog => "SessionLog",
        ManagedStorageSemanticOwnerV1::SessionLifecycleLog => "SessionLifecycleLog",
        ManagedStorageSemanticOwnerV1::InteractiveInputHistory => "InteractiveInputHistory",
        ManagedStorageSemanticOwnerV1::DurableMemory(_) => "DurableMemory",
        ManagedStorageSemanticOwnerV1::WorkspaceMutationState => "WorkspaceMutationState",
        ManagedStorageSemanticOwnerV1::ApplicationControlLog => "ApplicationControlLog",
        ManagedStorageSemanticOwnerV1::PlanStore => "PlanStore",
        ManagedStorageSemanticOwnerV1::SessionCatalog => "SessionCatalog",
        ManagedStorageSemanticOwnerV1::ProviderConnectionState => "ProviderConnectionState",
        ManagedStorageSemanticOwnerV1::AdapterDurableState(_) => "AdapterDurableState",
        ManagedStorageSemanticOwnerV1::RuntimeCache(_) => "RuntimeCache",
        ManagedStorageSemanticOwnerV1::ArtifactStaging => "ArtifactStaging",
        ManagedStorageSemanticOwnerV1::ArtifactStore => "ArtifactStore",
    }
}

pub fn kind_for_owner(owner: ManagedStorageSemanticOwnerV1) -> ResourceKindV1 {
    match owner {
        ManagedStorageSemanticOwnerV1::ArtifactStaging => ResourceKindV1::ArtifactStaging,
        ManagedStorageSemanticOwnerV1::ArtifactStore => ResourceKindV1::ArtifactStore,
        ManagedStorageSemanticOwnerV1::RuntimeCache(_) => ResourceKindV1::RuntimeCache,
        _ => ResourceKindV1::RuntimeState,
    }
}

/// Validates an admission request against the frozen matrix. Unknown combinations fail closed.
pub fn validate_matrix_admission(
    request: &ManagedStorageAdmissionRequestV1,
) -> Result<(), MatrixErrorV1> {
    let kind = kind_for_owner(request.semantic_owner);
    let mut matched = false;
    for cell in frozen_matrix() {
        if cell_matches(cell, request) {
            matched = true;
        }
    }
    if !matched || !kind_matches(kind, request) {
        return Err(MatrixErrorV1::UnknownCombination {
            owner: owner_label(request.semantic_owner).to_owned(),
            family: format!("{:?}", request.capability_family),
            source_label: format!("{:?}", request.source.source_class()),
        });
    }
    Ok(())
}

/// Closed matrix error classification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MatrixErrorV1 {
    #[error(
        "unknown owner x kind x capability x source x purpose combination: {owner} / {family} / {source_label}"
    )]
    UnknownCombination {
        owner: String,
        family: String,
        #[allow(dead_code)]
        source_label: String,
    },
}

#[cfg(test)]
mod tests {
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
}
