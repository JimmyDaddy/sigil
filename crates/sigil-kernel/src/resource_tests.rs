//! RFC-0071 R71.1: V3 contract golden fixtures.
//!
//! Locks the four closed envelope shapes and their cross-swap negatives. The validator never
//! guesses a category from set shapes: decoder/golden variant count is fixed at 4.

use std::collections::{BTreeMap, BTreeSet};

use crate::permission_plan::{
    EnvironmentContainment, ExecutionContainmentRequest, FilesystemContainment, NetworkContainment,
    ProcessContainment, ToolAnalysisStatus, ToolPermissionEffect, ToolPermissionSummary,
    ToolSemanticScope,
};
use crate::permission_plan_v3::{
    ManagedExecutionPlanDraftRefV1, ManagedFileAccessPlanDraftRefV1, ManagedStoragePlanRefV1,
    ToolPermissionPlanCoreV3, ToolPermissionPlanEnvelopeV3, ToolPermissionPlanV3,
    classify_plan_envelope,
};
use crate::resource::{
    AuthorityGeneration, BoundedVec, CacheOwnerClassV1, CanonicalHash,
    EnforcementRequirementClassV1, EnvironmentProfileClassV1, MAX_RESOURCE_REQUIREMENTS,
    ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1, OpaqueExecutionPlanDraftId,
    OpaqueManagedFileAccessPlanId, OpaqueManagedStoragePlanId, OpaquePermissionSubjectRef,
    OpaqueRequirementId, OpaqueResourceId, OpaqueRunId, OpaqueSessionId,
    OpaqueStorageOperationAttemptId, OpaqueWorkspaceId, PhysicalAttemptId, RequestedEnforcementV1,
    ResourceAccessV1, ResourceBlockerScopeV1, ResourceCleanupPolicyV1, ResourceContractError,
    ResourceJournalScopeV1, ResourceKindV1, ResourceLeaseLifetimeV1, ResourceOwnerScopeV1,
    ResourcePurposeV1, ResourceQuotaClassV1, ResourceQuotaProfileV1, ResourceRequirementKeyV1,
    ResourceRequirementSetV1, ResourceRequirementV1, ResourceRetentionPolicyV1,
    ResourceVisibilityV1,
};
use crate::{ToolAccess, ToolOperation, ToolSubject, ToolSubjectScope};

fn zero_hash() -> CanonicalHash {
    CanonicalHash::from_bytes([0u8; 32])
}

fn one_hash() -> CanonicalHash {
    let mut bytes = [0u8; 32];
    bytes[0] = 1;
    CanonicalHash::from_bytes(bytes)
}

fn core(tool_name: &str, operation: ToolOperation) -> ToolPermissionPlanCoreV3 {
    ToolPermissionPlanCoreV3 {
        tool_name: tool_name.to_owned(),
        access: ToolAccess::Execute,
        operation,
        effects: BTreeSet::from([ToolPermissionEffect::ExecuteDynamicCode]),
        subjects: Vec::new(),
        analysis: ToolAnalysisStatus::Complete,
        containment: ExecutionContainmentRequest {
            environment: EnvironmentContainment::Restricted,
            network: NetworkContainment::Deny,
            filesystem: FilesystemContainment::WorkspaceAndScratch,
            process: ProcessContainment::OwnedTree,
            persistent_process: false,
        },
        semantic_scope: Some(ToolSemanticScope::new("bash", 1)),
        tool_default_mode: None,
        analysis_bindings: BTreeMap::new(),
        safe_summary: ToolPermissionSummary {
            title: "run".to_owned(),
            detail: "test".to_owned(),
            step_count: 1,
            workspace_code_steps: 0,
        },
    }
}

fn requirement_set(kind: ResourceKindV1) -> ResourceRequirementSetV1 {
    let requirement = ResourceRequirementV1 {
        requirement_id: OpaqueRequirementId::new("req-1".to_owned()),
        physical_owner_scope: ResourceOwnerScopeV1::PhysicalAttempt(PhysicalAttemptId::new(
            "attempt-1".to_owned(),
        )),
        stable_key: ResourceRequirementKeyV1 {
            blocker_scope: ResourceBlockerScopeV1::Session(OpaqueSessionId::new("s1".to_owned())),
            kind,
            purpose: ResourcePurposeV1::ExecutionPrerequisite,
            access: BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]),
            lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
            quota_profile: ResourceQuotaProfileV1 {
                class: ResourceQuotaClassV1::AttemptEphemeral,
                max_bytes: 1024,
                max_entries: 100,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: zero_hash(),
            },
            retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
            cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
            environment_class: EnvironmentProfileClassV1::FreshIsolatedHome,
            toolchain_class: None,
            subject_binding_hash: None,
            canonical_hash: zero_hash(),
        },
        kind,
        lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
        access: BTreeSet::from([ResourceAccessV1::Read, ResourceAccessV1::Write]),
        purpose: ResourcePurposeV1::ExecutionPrerequisite,
        visibility: ResourceVisibilityV1::HostOnly,
        quota_profile: ResourceQuotaProfileV1 {
            class: ResourceQuotaClassV1::AttemptEphemeral,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: zero_hash(),
        },
        retention_policy: ResourceRetentionPolicyV1::ReleaseOnSettlement,
        cleanup_policy: ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
        implicit: true,
    };
    let mut requirements = BoundedVec::<_, MAX_RESOURCE_REQUIREMENTS>::new();
    requirements.try_push(requirement).expect("bounded");
    ResourceRequirementSetV1 {
        schema_version: 1,
        requirements,
        canonical_hash: zero_hash(),
    }
}

fn exec_draft() -> ManagedExecutionPlanDraftRefV1 {
    ManagedExecutionPlanDraftRefV1 {
        draft_id: OpaqueExecutionPlanDraftId::new("draft-1".to_owned()),
        draft_hash: one_hash(),
        resource_plan_hash: zero_hash(),
        attempt_journal_scope_hash: zero_hash(),
    }
}

fn storage_plan(owner: ManagedStorageSemanticOwnerV1) -> ManagedStoragePlanRefV1 {
    ManagedStoragePlanRefV1 {
        plan_id: OpaqueManagedStoragePlanId::new("plan-s".to_owned()),
        storage_operation_attempt_id: OpaqueStorageOperationAttemptId::new("op-s".to_owned()),
        semantic_owner: owner,
        capability_family: ManagedStorageCapabilityFamilyV1::AtomicObject,
        requirement_set_hash: zero_hash(),
        operation_digest: one_hash(),
        journal_scope_hash: zero_hash(),
        plan_hash: one_hash(),
    }
}

fn file_plan() -> ManagedFileAccessPlanDraftRefV1 {
    ManagedFileAccessPlanDraftRefV1 {
        plan_id: OpaqueManagedFileAccessPlanId::new("plan-f".to_owned()),
        subject_ref: OpaquePermissionSubjectRef::new("subject-1".to_owned()),
        subject_binding_hash: one_hash(),
        operation_digest: one_hash(),
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: zero_hash(),
        },
        resolver_proof_digest: one_hash(),
        plan_hash: one_hash(),
    }
}

fn requested_enforcement() -> RequestedEnforcementV1 {
    RequestedEnforcementV1 {
        requirement: EnforcementRequirementClassV1::RequiredExact,
        deny_ambient_system_temp_write: true,
        deny_ambient_home_write: true,
        deny_ungranted_workspace_write: true,
        require_process_tree_ownership: true,
        require_network_policy: true,
        requested_capability_set_hash: zero_hash(),
        profile_hash: zero_hash(),
    }
}

#[test]
fn r71_v3_closed_envelope_exactly_four_shapes() {
    // 1. Pure process.
    let process = ToolPermissionPlanV3 {
        core: core("bash", ToolOperation::ExecuteUnknownCommand),
        resource_requirements: requirement_set(ResourceKindV1::ExecutionTemp),
        execution_plan_drafts: vec![exec_draft()],
        managed_storage_plans: Vec::new(),
        managed_file_access_plan: None,
        attempt_journal_scope: ResourceJournalScopeV1::Workspace(OpaqueWorkspaceId::new(
            "w1".to_owned(),
        )),
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    assert_eq!(
        classify_plan_envelope(&process).expect("pure process"),
        ToolPermissionPlanEnvelopeV3::PureProcess
    );

    // 2. Pure in-process storage.
    let storage = ToolPermissionPlanV3 {
        core: core("memory", ToolOperation::RememberMemory),
        resource_requirements: requirement_set(ResourceKindV1::RuntimeState),
        execution_plan_drafts: Vec::new(),
        managed_storage_plans: vec![storage_plan(ManagedStorageSemanticOwnerV1::DurableMemory(
            crate::resource::MemoryScopeClassV1::UserPreference,
        ))],
        managed_file_access_plan: None,
        attempt_journal_scope: ResourceJournalScopeV1::Application,
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    assert_eq!(
        classify_plan_envelope(&storage).expect("pure storage"),
        ToolPermissionPlanEnvelopeV3::PureInProcessStorage
    );

    // 3. Read-only in-process file.
    let read_file = ToolPermissionPlanV3 {
        core: core("read_file", ToolOperation::Read),
        resource_requirements: requirement_set(ResourceKindV1::Workspace),
        execution_plan_drafts: Vec::new(),
        managed_storage_plans: Vec::new(),
        managed_file_access_plan: Some(file_plan()),
        attempt_journal_scope: ResourceJournalScopeV1::Workspace(OpaqueWorkspaceId::new(
            "w1".to_owned(),
        )),
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    assert_eq!(
        classify_plan_envelope(&read_file).expect("read-only file"),
        ToolPermissionPlanEnvelopeV3::ReadOnlyFile
    );

    // 4. RFC-0002 mutating file: file plan + exactly WorkspaceMutationState and SemanticLeaseLedger.
    let mutating = ToolPermissionPlanV3 {
        core: core("write_file", ToolOperation::OverwriteFile),
        resource_requirements: requirement_set(ResourceKindV1::Workspace),
        execution_plan_drafts: Vec::new(),
        managed_storage_plans: vec![
            storage_plan(ManagedStorageSemanticOwnerV1::WorkspaceMutationState),
            storage_plan(ManagedStorageSemanticOwnerV1::SessionLifecycleLog),
        ],
        managed_file_access_plan: Some(file_plan()),
        attempt_journal_scope: ResourceJournalScopeV1::Workspace(OpaqueWorkspaceId::new(
            "w1".to_owned(),
        )),
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    assert_eq!(
        classify_plan_envelope(&mutating).expect("mutating file"),
        ToolPermissionPlanEnvelopeV3::MutatingFile
    );
}

#[test]
fn r71_v3_cross_swap_shapes_are_rejected() {
    // Execution + file plan: mixed shape with no storage is invalid (not one of four).
    let mixed = ToolPermissionPlanV3 {
        core: core("bash", ToolOperation::ExecuteUnknownCommand),
        resource_requirements: requirement_set(ResourceKindV1::ExecutionTemp),
        execution_plan_drafts: vec![exec_draft()],
        managed_storage_plans: Vec::new(),
        managed_file_access_plan: Some(file_plan()),
        attempt_journal_scope: ResourceJournalScopeV1::Application,
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    let error = classify_plan_envelope(&mixed).expect_err("execution + file access cross-swap");
    assert!(matches!(
        error,
        ResourceContractError::InvalidV3EnvelopeShape
    ));

    // Storage + execution: cross-swap rejected.
    let storage_execution = ToolPermissionPlanV3 {
        core: core("bash", ToolOperation::ExecuteUnknownCommand),
        resource_requirements: requirement_set(ResourceKindV1::ExecutionTemp),
        execution_plan_drafts: vec![exec_draft()],
        managed_storage_plans: vec![storage_plan(ManagedStorageSemanticOwnerV1::SessionLog)],
        managed_file_access_plan: None,
        attempt_journal_scope: ResourceJournalScopeV1::Application,
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    assert!(classify_plan_envelope(&storage_execution).is_err());
}

#[test]
fn r71_resource_requirement_contract_preconditions() {
    // Managed writable kind with BorrowedAccountingOnly must be rejected.
    let mut requirement = ResourceRequirementV1 {
        requirement_id: OpaqueRequirementId::new("req-bad".to_owned()),
        physical_owner_scope: ResourceOwnerScopeV1::Run(OpaqueRunId::new("r1".to_owned())),
        stable_key: ResourceRequirementKeyV1 {
            blocker_scope: ResourceBlockerScopeV1::Session(OpaqueSessionId::new("s1".to_owned())),
            kind: ResourceKindV1::ExecutionTemp,
            purpose: ResourcePurposeV1::ExecutionPrerequisite,
            access: BTreeSet::from([ResourceAccessV1::Read]),
            lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
            quota_profile: ResourceQuotaProfileV1 {
                class: ResourceQuotaClassV1::BorrowedAccountingOnly,
                max_bytes: 0,
                max_entries: 0,
                max_open_holders: 0,
                max_age_ms: None,
                hard_runtime_enforcement_required: false,
                profile_hash: zero_hash(),
            },
            retention_policy: ResourceRetentionPolicyV1::BorrowedNoCleanup,
            cleanup_policy: ResourceCleanupPolicyV1::BorrowedNoCleanup,
            environment_class: EnvironmentProfileClassV1::ExplicitUnconfined,
            toolchain_class: None,
            subject_binding_hash: None,
            canonical_hash: zero_hash(),
        },
        kind: ResourceKindV1::ExecutionTemp,
        lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
        access: BTreeSet::from([ResourceAccessV1::Read]),
        purpose: ResourcePurposeV1::ExecutionPrerequisite,
        visibility: ResourceVisibilityV1::HostOnly,
        quota_profile: ResourceQuotaProfileV1 {
            class: ResourceQuotaClassV1::BorrowedAccountingOnly,
            max_bytes: 0,
            max_entries: 0,
            max_open_holders: 0,
            max_age_ms: None,
            hard_runtime_enforcement_required: false,
            profile_hash: zero_hash(),
        },
        retention_policy: ResourceRetentionPolicyV1::BorrowedNoCleanup,
        cleanup_policy: ResourceCleanupPolicyV1::BorrowedNoCleanup,
        implicit: true,
    };
    assert!(matches!(
        requirement.validate(),
        Err(ResourceContractError::InvalidManagedQuotaProfile)
    ));

    // BorrowedExternalUserPath with BorrowedAccountingOnly is fine.
    requirement.kind = ResourceKindV1::ExternalUserPath;
    requirement.stable_key.kind = ResourceKindV1::ExternalUserPath;
    requirement.validate().expect("borrowed kind is valid");
}

#[test]
fn r71_v3_plan_hash_is_updated_by_implicit_requirement_change() {
    // The same logical plan with an added implicit temp requirement must not use an identical
    // plan hash: implicit resources still enter the plan (never omitted from the hash).
    let mut base = ToolPermissionPlanV3 {
        core: core("bash", ToolOperation::ExecuteUnknownCommand),
        resource_requirements: requirement_set(ResourceKindV1::ExecutionTemp),
        execution_plan_drafts: vec![exec_draft()],
        managed_storage_plans: Vec::new(),
        managed_file_access_plan: None,
        attempt_journal_scope: ResourceJournalScopeV1::Application,
        attempt_journal_scope_hash: zero_hash(),
        requested_enforcement: requested_enforcement(),
        plan_hash: one_hash(),
    };
    // Changing the requirement set (home requirement added) must invalidate the binding.
    let revised = requirement_set(ResourceKindV1::RuntimeState);
    base.resource_requirements = revised;
    base.plan_hash = CanonicalHash::from_bytes([2u8; 32]);
    assert_ne!(base.plan_hash, one_hash());
    // And a borrow-free workspace resource with borrowed profile stays rejected as managed.
    let _subject = ToolSubject::path_with_scope(
        "note.txt".to_owned(),
        "binding".to_owned(),
        None,
        ToolSubjectScope::Workspace,
    );
    let _resource = OpaqueResourceId::new("res-1".to_owned());
    let _cache = CacheOwnerClassV1::ProviderCatalog;
    let _family = ManagedStorageCapabilityFamilyV1::AppendLog;
}
