//! E02-b fault coverage for the authenticated process-inventory attachment boundary.
//!
//! These tests use only an owned temporary bootstrap root and an owned child. They exercise the
//! production observer/factory path rather than a PID-shaped fixture.

use crate::{
    AuthorityBootstrapObjectClassV1, AuthorityBootstrapStoreV1, AuthorityManagedProcessInventoryV1,
    AuthorityProcessInventoryBootstrapBindingV1, AuthorityProcessInventoryErrorV1,
    AuthorityProcessInventoryPortV1, AuthorityProcessSpawnRequestV1,
};
use sigil_kernel::{
    process_observation::HostProcessIdentityRegistrationV1,
    resource::{CanonicalHash, PhysicalAttemptId},
};

fn hash(bytes: &[u8]) -> CanonicalHash {
    crate::bootstrap::canonical_bootstrap_hash(bytes)
}

fn factory()
-> std::sync::Arc<dyn sigil_kernel::process_observation::HostProcessObservationFactoryV1> {
    sigil_process_observer::ProcessObserverFactoryV1::new(hash(
        b"r71-e02-process-inventory-fault-test-observer",
    ))
    .expect("current test process is observable")
    .instantiate()
}

fn binding(composition_epoch: u64) -> AuthorityProcessInventoryBootstrapBindingV1 {
    AuthorityProcessInventoryBootstrapBindingV1 {
        application_composition_epoch: composition_epoch,
        owner_execution_scope_hash: hash(b"r71-e02-process-inventory-fault-test-owner"),
    }
}

fn request(attempt: &str) -> AuthorityProcessSpawnRequestV1 {
    AuthorityProcessSpawnRequestV1 {
        attempt_id: PhysicalAttemptId::new(attempt.to_owned()),
        execution_scope_hash: hash(attempt.as_bytes()),
    }
}

#[cfg(unix)]
fn spawn_owned_child() -> std::io::Result<std::process::Child> {
    std::process::Command::new("sleep").arg("1").spawn()
}

#[cfg(windows)]
fn spawn_owned_child() -> std::io::Result<std::process::Child> {
    std::process::Command::new("ping")
        .args(["127.0.0.1", "-n", "2"])
        .spawn()
}

#[test]
fn r71_e02_failed_child_attach_then_reap_and_settle_keeps_weak_coverage_history() {
    let temp = tempfile::tempdir().expect("temporary authority root");
    let store =
        AuthorityBootstrapStoreV1::open_owned_temp_fixture(&temp, "e02-failed-child-attach", 1)
            .expect("fresh owned temporary store");
    let publication = store.acquire_publication().expect("publication");
    let inventory = AuthorityManagedProcessInventoryV1::initialize(
        store.clone(),
        &publication,
        binding(1),
        factory(),
    )
    .expect("initialize authenticated inventory");
    drop(publication);

    let claim = inventory
        .prepare_spawn(request("failed-attach-child"))
        .expect("prepare");
    let mut child = spawn_owned_child().expect("owned child");
    let registration = claim
        .register_spawned_process(child.id())
        .expect("real live-birth registration");
    let forged = HostProcessIdentityRegistrationV1::new(
        registration.identity().clone(),
        "foreign-issuance".to_owned(),
        registration.observed_at_monotonic_ms(),
    );
    assert!(matches!(
        inventory.attach_spawn(&claim, forged),
        Err(AuthorityProcessInventoryErrorV1::Observation(_))
    ));

    child.kill().expect("terminate owned child");
    child.wait().expect("reap owned child");
    inventory
        .settle_spawn(claim)
        .expect("settle prepared claim");

    let publication = store.acquire_publication().expect("inspection publication");
    let snapshot_bytes = store
        .read_bytes(
            &publication,
            AuthorityBootstrapObjectClassV1::ProcessInventory,
        )
        .expect("read authenticated inventory")
        .expect("inventory exists");
    let snapshot = crate::process_inventory::decode_snapshot(&snapshot_bytes)
        .expect("decode authenticated inventory");
    let authenticator_bytes = store
        .read_bytes(
            &publication,
            AuthorityBootstrapObjectClassV1::ProcessInventoryAuthenticator,
        )
        .expect("read durable authenticator")
        .expect("authenticator exists");
    let authenticator = crate::process_inventory::decode_authenticator(&authenticator_bytes)
        .expect("decode durable authenticator");
    snapshot
        .validate(store.authority_epoch(), &authenticator)
        .expect("history remains authenticated");
    assert!(
        snapshot.entries.is_empty(),
        "settlement clears only active claim"
    );
    assert_eq!(snapshot.bounded_native_exposure_count, 1);
    assert!(snapshot.bounded_native_exposure_frontier.is_some());
    assert!(
        store
            .read_bytes(
                &publication,
                AuthorityBootstrapObjectClassV1::ProcessInventory
            )
            .expect("inventory bytes")
            .is_some(),
        "settlement must retain an authenticated record rather than deleting it"
    );
}

#[test]
fn r71_e02_same_birth_recomposition_fences_old_inventory_from_new_prepares() {
    let temp = tempfile::tempdir().expect("temporary authority root");
    let store =
        AuthorityBootstrapStoreV1::open_owned_temp_fixture(&temp, "e02-same-owner-recompose", 1)
            .expect("fresh owned temporary store");
    let publication = store.acquire_publication().expect("initial publication");
    let old_inventory = AuthorityManagedProcessInventoryV1::initialize(
        store.clone(),
        &publication,
        binding(1),
        factory(),
    )
    .expect("initial inventory");
    drop(publication);

    let publication = store
        .acquire_publication()
        .expect("recomposition publication");
    let new_inventory =
        AuthorityManagedProcessInventoryV1::initialize(store, &publication, binding(2), factory())
            .expect("same OS birth may recompose into a new scope");
    drop(publication);

    assert!(matches!(
        old_inventory.prepare_spawn(request("old-handle-new-attempt")),
        Err(AuthorityProcessInventoryErrorV1::InvalidClaim)
    ));
    let fresh_claim = new_inventory
        .prepare_spawn(request("new-handle-new-attempt"))
        .expect("new composition may prepare its own attempt");
    new_inventory
        .settle_spawn(fresh_claim)
        .expect("new composition settles its exact claim");
}
