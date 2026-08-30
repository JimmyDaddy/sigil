use super::*;

fn plan() -> ToolchainBindingPlanV1 {
    ToolchainBindingPlanV1 {
        family: ToolchainFamilyV1::Rust,
        executable_observation: BorrowedResourceObservationV1 {
            subject_ref: "toolchain:cargo".to_owned(),
            safe_label: "cargo".to_owned(),
            identity_digest: CanonicalHash::from_bytes([1u8; 32]),
        },
        readonly_store_observations: Vec::new(),
        managed_requirement_keys: vec!["tool-cache-rust".to_owned()],
        user_config_source_observations: Vec::new(),
        environment_plan: vec![PlannedEnvironmentVariableV1 {
            name: "CARGO_HOME".to_owned(),
            source: PlannedEnvSourceV1::ToolCache,
        }],
        plan_hash: CanonicalHash::from_bytes([2u8; 32]),
    }
}

#[test]
fn r71_toolchain_realized_binding_must_match_plan_cas() {
    let plan = plan();
    let mut realized = RealizedToolchainBindingV1 {
        family: ToolchainFamilyV1::Rust,
        executable_ref: ResourceRefV1 {
            resource_id: OpaqueResourceId::new("exec-1".to_owned()),
            kind: ResourceKindV1::ToolchainStore,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
            generation: 1,
        },
        readonly_stores: Vec::new(),
        managed_cache_refs: Vec::new(),
        safe_user_config_views: Vec::new(),
        environment: plan.environment_plan.clone(),
        source_plan_hash: plan.plan_hash,
        binding_hash: CanonicalHash::from_bytes([3u8; 32]),
    };
    validate_realized_binding(&plan, &realized).expect("match");
    realized.source_plan_hash = CanonicalHash::from_bytes([9u8; 32]);
    let error = validate_realized_binding(&plan, &realized).expect_err("drift");
    assert!(matches!(error, ToolchainErrorV1::BindingDrift));
}
