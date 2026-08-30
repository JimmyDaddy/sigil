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
#[path = "tests/semantic_matrix_tests.rs"]
mod tests;
