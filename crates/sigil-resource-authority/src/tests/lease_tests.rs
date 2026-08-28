use super::*;

fn record() -> ManagedGenerationRecordV1 {
    ManagedGenerationRecordV1 {
        resource_id: "res-1".to_owned(),
        generation: 1,
        state: ResourceGenerationStateV1::Planned,
        authority_generation: AuthorityGeneration {
            epoch: 1,
            instance_hash: CanonicalHash::from_bytes([0u8; 32]),
        },
        journal_scope: ResourceJournalScopeV1::Application,
        physical_attempt_id: None,
        bound_manifest_hash: None,
        holder_count: 0,
        cleanup_status: ResourceCleanupStatusV1::NotStarted,
        journal_frontier_hash: CanonicalHash::from_bytes([0u8; 32]),
    }
}

#[test]
fn r71_lease_planned_never_jumps_to_active() {
    let mut rec = record();
    let error = rec
        .transition(ResourceGenerationStateV1::Active)
        .expect_err("must fail");
    assert!(matches!(error, LeaseTransitionErrorV1::PlannedToActive));
}

#[test]
fn r71_lease_ready_without_binding_may_not_spawn() {
    let mut rec = record();
    rec.transition(ResourceGenerationStateV1::Provisioning)
        .expect("provision");
    rec.transition(ResourceGenerationStateV1::Ready)
        .expect("ready");
    let error = rec
        .transition(ResourceGenerationStateV1::Active)
        .expect_err("must fail");
    assert!(matches!(error, LeaseTransitionErrorV1::UnboundReadySpawn));
}

#[test]
fn r71_lease_active_must_settle_before_retry_paths() {
    let mut rec = record();
    rec.transition(ResourceGenerationStateV1::Provisioning)
        .expect("provision");
    rec.transition(ResourceGenerationStateV1::Ready)
        .expect("ready");
    rec.bound_manifest_hash = Some(CanonicalHash::from_bytes([1u8; 32]));
    rec.transition(ResourceGenerationStateV1::Bound)
        .expect("bound");
    rec.transition(ResourceGenerationStateV1::Active)
        .expect("active");
    let error = rec
        .transition(ResourceGenerationStateV1::Planned)
        .expect_err("no replay");
    assert!(matches!(
        error,
        LeaseTransitionErrorV1::IllegalTransition {
            from: "active",
            to: "planned"
        }
    ));
}

#[test]
fn r71_lease_quarantined_never_reactivates() {
    let mut rec = record();
    rec.transition(ResourceGenerationStateV1::Provisioning)
        .expect("provision");
    rec.transition(ResourceGenerationStateV1::Ready)
        .expect("ready");
    rec.bound_manifest_hash = Some(CanonicalHash::from_bytes([1u8; 32]));
    rec.transition(ResourceGenerationStateV1::Bound)
        .expect("bound");
    rec.transition(ResourceGenerationStateV1::Active)
        .expect("active");
    rec.holder_count = 0;
    rec.transition(ResourceGenerationStateV1::Finalizing)
        .expect("finalizing");
    rec.transition(ResourceGenerationStateV1::Quarantined)
        .expect("quarantine");
    let error = rec
        .transition(ResourceGenerationStateV1::Ready)
        .expect_err("no reactivate");
    assert!(matches!(
        error,
        LeaseTransitionErrorV1::QuarantinedReactivation
    ));
}
