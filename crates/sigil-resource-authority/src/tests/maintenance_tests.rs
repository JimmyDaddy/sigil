use super::*;

fn h(seed: u8) -> CanonicalHash {
    CanonicalHash::from_bytes([seed; 32])
}

fn plan(count: u64) -> ResourceMaintenancePlanV1 {
    ResourceMaintenancePlanV1 {
        intent_hash: sigil_kernel::resource::CanonicalHash::from_bytes([1u8; 32]),
        selected_resource_refs_hash: sigil_kernel::resource::CanonicalHash::from_bytes([2u8; 32]),
        selected_count: count,
        selected_bytes: 42,
        authority_generation: AuthorityGeneration {
            epoch: 7,
            instance_hash: sigil_kernel::resource::CanonicalHash::from_bytes([3u8; 32]),
        },
        plan_hash: sigil_kernel::resource::CanonicalHash::from_bytes([4u8; 32]),
    }
}

fn proof(for_plan: &ResourceMaintenancePlanV1) -> ResourceMaintenanceAuthorizationProofV1 {
    ResourceMaintenanceAuthorizationProofV1 {
        source: ResourceMaintenanceAuthorizationSourceV1::RetentionEligibility {
            policy_digest: sigil_kernel::resource::CanonicalHash::from_bytes([5u8; 32]),
            evaluated_frontier: 1,
            eligibility_proof_digest: sigil_kernel::resource::CanonicalHash::from_bytes([6u8; 32]),
        },
        plan_hash: for_plan.plan_hash,
        expected_authority_generation: for_plan.authority_generation,
        proof_hash: sigil_kernel::resource::CanonicalHash::from_bytes([7u8; 32]),
    }
}

#[test]
fn r71_maintenance_empty_selection_is_rejected_before_delete() {
    let plan = plan(0);
    let proof = proof(&plan);
    let error = validate_maintenance_binding(&plan, &proof).expect_err("empty must fail");
    assert!(matches!(error, MaintenanceErrorV1::EmptySelection));
}

#[test]
fn r71_maintenance_generation_drift_fails_closed() {
    let mut drifted = plan(1);
    drifted.authority_generation.epoch = 9;
    // Proof constructed from the ORIGINAL plan generation, then plan drifts after the proof.
    let original = plan(1);
    let proof = proof(&original);
    let error = validate_maintenance_binding(&drifted, &proof).expect_err("drift must fail");
    assert!(matches!(error, MaintenanceErrorV1::GenerationDrift));
}

#[test]
fn r71_maintenance_token_claim_is_one_shot() {
    let plan = plan(1);
    let proof = proof(&plan);
    validate_maintenance_binding(&plan, &proof).expect("valid");
    let mut token = ResourceMaintenanceTokenV1::new(plan, proof);
    token.consume_claim().expect("first");
    let error = token.consume_claim().expect_err("second must fail");
    assert!(matches!(error, MaintenanceErrorV1::DuplicateClaim));
}

#[test]
fn r71_artifact_retire_authority_binds_paired_grants_and_one_shots() {
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: h(20),
    };
    let authority = ArtifactRetireAuthorityV1::new(generation, h(21), h(22));
    let evidence = ArtifactRetireEligibilityEvidenceV1 {
        authority_generation: generation,
        artifact_staging_grant_hash: h(21),
        artifact_store_grant_hash: h(22),
        selected_refs_hash: h(23),
        selected_count: 1,
        selected_bytes: 0,
        eligibility_frontier: 7,
        policy_hash: h(24),
    };
    let mut token = authority.authorize(evidence).expect("authorize");
    assert_ne!(token.token_hash(), h(0));
    token.consume_claim().expect("first claim");
    assert!(matches!(
        token.consume_claim(),
        Err(MaintenanceErrorV1::DuplicateClaim)
    ));
}

#[test]
fn r71_artifact_retire_authority_rejects_cross_grant_and_stale_frontier() {
    let generation = AuthorityGeneration {
        epoch: 1,
        instance_hash: h(30),
    };
    let authority = ArtifactRetireAuthorityV1::new(generation, h(31), h(32));
    let mut evidence = ArtifactRetireEligibilityEvidenceV1 {
        authority_generation: generation,
        artifact_staging_grant_hash: h(31),
        artifact_store_grant_hash: h(99),
        selected_refs_hash: h(33),
        selected_count: 1,
        selected_bytes: 1,
        eligibility_frontier: 1,
        policy_hash: h(34),
    };
    assert!(matches!(
        authority.authorize(evidence.clone()),
        Err(MaintenanceErrorV1::GrantMismatch)
    ));
    evidence.artifact_store_grant_hash = h(32);
    evidence.eligibility_frontier = 0;
    assert!(matches!(
        authority.authorize(evidence),
        Err(MaintenanceErrorV1::EligibilityFrontierMissing)
    ));
}
