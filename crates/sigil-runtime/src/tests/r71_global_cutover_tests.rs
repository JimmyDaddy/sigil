use super::*;
use sigil_kernel::capability_issuer::{KernelCapabilityIssuerV1, mock_issuer};
use sigil_resource_authority::storage::{
    AuthorityManagedStorageServiceV1, AuthorityStorageGrantTableV1,
};
use std::sync::Arc;

fn authority() -> AuthorityGeneration {
    AuthorityGeneration {
        epoch: 2,
        instance_hash: CanonicalHash::from_bytes([2u8; 32]),
    }
}

fn shadow_services(issuer: Arc<dyn KernelCapabilityIssuerV1>) -> RuntimeManagedResourceServicesV1 {
    shadow_services_with_table(issuer, AuthorityStorageGrantTableV1::new())
}

fn shadow_services_with_table(
    issuer: Arc<dyn KernelCapabilityIssuerV1>,
    table: AuthorityStorageGrantTableV1,
) -> RuntimeManagedResourceServicesV1 {
    let storage = Arc::new(AuthorityManagedStorageServiceV1::new(table, authority()));
    let file_access = sigil_resource_authority::file_access_stub::stub_file_access_service();
    let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new(
        authority(),
        storage,
        file_access,
    )
    .build_bundle();
    RuntimeManagedResourceServicesV1::compose(
        bundle,
        issuer,
        Arc::new(CutoverStubProjectionServiceV1),
    )
}

struct CutoverStubProjectionServiceV1;

#[async_trait::async_trait]
impl sigil_kernel::managed_projection::ManagedProjectionServiceV1
    for CutoverStubProjectionServiceV1
{
    async fn open_rebuildable_projection(
        &self,
        _handle: &sigil_kernel::managed_storage::ManagedStorageNamespaceHandleV1,
        _request: sigil_kernel::managed_projection::OpenProjectionConnectionRequestV1,
    ) -> Result<
        Box<dyn sigil_kernel::managed_projection::ManagedProjectionConnectionV1>,
        sigil_kernel::managed_projection::ProjectionErrorV1,
    > {
        Err(sigil_kernel::managed_projection::ProjectionErrorV1::ConnectionClosed)
    }
}

#[test]
fn resource_global_cutover_shadow_surface_fails_closed_on_new_epoch() {
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-shadow",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    let error = cutover.gate().expect_err("must fail closed");
    assert!(matches!(error, CutoverErrorV1::AdapterNotReady(_)));
    assert!(!cutover.is_current_schema_ready());
    assert_eq!(cutover.manifest().mandatory_readiness.len(), 18);
}

#[test]
fn resource_global_cutover_extension_probe_reflects_real_seam() {
    let mut services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let before = RuntimeGlobalCutoverV1::evaluate(
        "inst-ext-before",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    let ext_before = before
        .manifest()
        .mandatory_readiness
        .iter()
        .find(|probe| probe.adapter == MandatoryAdapterKindV1::ExecutionExtension)
        .expect("extension probe");
    assert!(
        !ext_before.passed,
        "legacy launcher must not claim the extension route"
    );

    services.extension_execution_seam = RuntimeExecutionExtensionSeamV1::ManagedExecutionBacked;
    let after = RuntimeGlobalCutoverV1::evaluate(
        "inst-ext-after",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    let ext_after = after
        .manifest()
        .mandatory_readiness
        .iter()
        .find(|probe| probe.adapter == MandatoryAdapterKindV1::ExecutionExtension)
        .expect("extension probe");
    assert!(ext_after.passed, "the probe must reflect the composed seam");
}

#[test]
fn resource_global_cutover_legacy_epoch_requires_no_probes() {
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-legacy",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    assert!(cutover.gate().is_ok());
    assert_eq!(cutover.manifest().mandatory_readiness.len(), 0);
}

#[test]
fn resource_global_cutover_production_rehydration_rejects_legacy_manifest() {
    let legacy = RuntimeGlobalCutoverV1::legacy_decision("inst-legacy", 1, authority());
    let error = RuntimeGlobalCutoverV1::from_validated_manifest(legacy.manifest().clone())
        .expect_err("legacy must remain diagnostic-only");
    assert_eq!(error, CutoverErrorV1::AuthorityUnavailable);
}

#[test]
fn resource_global_cutover_storage_roundtrip_probe_is_real() {
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    // Empty grant table: every storage family probe must fail (service says mismatch).
    let probes = probe_mandatory_adapters(
        &services,
        &recovery,
        CanonicalHash::from_bytes([0xd1; 32]),
        1,
    );
    for probe in probes.iter().filter(|p| {
        matches!(
            p.adapter,
            MandatoryAdapterKindV1::StorageSessionLog
                | MandatoryAdapterKindV1::StorageSessionLifecycle
                | MandatoryAdapterKindV1::StorageInputHistory
                | MandatoryAdapterKindV1::StorageMemory
                | MandatoryAdapterKindV1::StorageSessionCatalog
                | MandatoryAdapterKindV1::StorageArtifact
                | MandatoryAdapterKindV1::StorageAdapterDurableState
        )
    }) {
        assert!(
            !probe.passed,
            "{:?} must fail without grants",
            probe.adapter
        );
    }
}

fn storage_grant(
    grant_id: &str,
    owner: sigil_kernel::resource::ManagedStorageSemanticOwnerV1,
    family: sigil_kernel::resource::ManagedStorageCapabilityFamilyV1,
) -> sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
    sigil_kernel::managed_storage::StorageAdmissionGrantV1 {
        grant_id: sigil_kernel::resource::OpaqueStorageGrantId::new(grant_id.to_owned()),
        admission_hash: CanonicalHash::from_bytes([0x31; 32]),
        semantic_owner: owner,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        purpose_hash: CanonicalHash::from_bytes([0x32; 32]),
        source_class: sigil_kernel::resource::StorageAdmissionSourceClassV1::ApplicationCutoverRoot,
        source_binding_hash: CanonicalHash::from_bytes([0x39; 32]),
        namespace_hash: {
            let mut ns = [0x33u8; 32];
            for (index, byte) in grant_id.bytes().take(16).enumerate() {
                ns[index] = byte;
            }
            CanonicalHash::from_bytes(ns)
        },
        journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
        journal_scope_hash: CanonicalHash::from_bytes([0x34; 32]),
        resource_ref: sigil_kernel::resource::ResourceRefV1 {
            resource_id: sigil_kernel::resource::OpaqueResourceId::new(format!("res-{grant_id}")),
            kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
            owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
            journal_scope: sigil_kernel::resource::ResourceJournalScopeV1::Application,
            generation: 1,
        },
        resource_binding_digest: CanonicalHash::from_bytes([0x35; 32]),
        physical_binding_hash: CanonicalHash::from_bytes([0x36; 32]),
        resource_kind: sigil_kernel::resource::ResourceKindV1::RuntimeState,
        owner_scope: sigil_kernel::resource::ResourceOwnerScopeV1::Application,
        capability_family: family,
        retention_policy: sigil_kernel::resource::ResourceRetentionPolicyV1::SessionPolicy,
        quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
            class: sigil_kernel::resource::ResourceQuotaClassV1::RuntimeState,
            max_bytes: 1024,
            max_entries: 100,
            max_open_holders: 1,
            max_age_ms: None,
            hard_runtime_enforcement_required: true,
            profile_hash: CanonicalHash::from_bytes([0x37; 32]),
        },
        semantic_schema: sigil_kernel::resource::OpaqueSemanticSchemaId::new(format!(
            "schema-{grant_id}"
        )),
        authority_generation: authority(),
        journal_admission_sequence: 1,
        grant_hash: CanonicalHash::from_bytes([0x38; 32]),
    }
}

#[test]
fn resource_global_cutover_storage_family_exact_probe() {
    use sigil_kernel::resource::{
        ManagedStorageCapabilityFamilyV1 as Family, ManagedStorageSemanticOwnerV1 as Owner,
    };
    let mut table = AuthorityStorageGrantTableV1::new();
    table
        .register(storage_grant(
            "g-session-log",
            Owner::SessionLog,
            Family::AppendLog,
        ))
        .expect("register");
    table
        .register(storage_grant(
            "g-input-history",
            Owner::InteractiveInputHistory,
            Family::AppendLog,
        ))
        .expect("register");
    table
        .register(storage_grant(
            "g-artifact",
            Owner::ArtifactStaging,
            Family::StreamingArtifact,
        ))
        .expect("register");
    let services = shadow_services_with_table(mock_issuer(), table);
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let probes = probe_mandatory_adapters(
        &services,
        &recovery,
        CanonicalHash::from_bytes([0xd2; 32]),
        1,
    );
    let passed: Vec<MandatoryAdapterKindV1> = probes
        .iter()
        .filter(|p| p.passed)
        .map(|p| p.adapter)
        .collect();
    // Exactly the three registered writer channels are ready; the other four fail closed.
    assert!(passed.contains(&MandatoryAdapterKindV1::StorageSessionLog));
    assert!(passed.contains(&MandatoryAdapterKindV1::StorageInputHistory));
    assert!(passed.contains(&MandatoryAdapterKindV1::StorageArtifact));
    assert!(!passed.contains(&MandatoryAdapterKindV1::StorageSessionLifecycle));
    assert!(!passed.contains(&MandatoryAdapterKindV1::StorageMemory));
    assert!(!passed.contains(&MandatoryAdapterKindV1::StorageSessionCatalog));
    assert!(!passed.contains(&MandatoryAdapterKindV1::StorageAdapterDurableState));
}

#[test]
fn resource_global_cutover_manifest_is_content_addressed() {
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let a = RuntimeGlobalCutoverV1::evaluate(
        "inst-ca",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    let b = RuntimeGlobalCutoverV1::evaluate(
        "inst-ca",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    assert_eq!(a.manifest().manifest_hash, b.manifest().manifest_hash);
}

struct CutoverTestDisclosurePresenter;

#[async_trait::async_trait]
impl sigil_kernel::egress::EgressDisclosurePresenter for CutoverTestDisclosurePresenter {
    async fn present(
        &self,
        _disclosure: sigil_kernel::egress::PreEgressDisclosure,
    ) -> Result<
        sigil_kernel::egress::DisclosurePresentationReceipt,
        sigil_kernel::egress::DisclosurePresentationError,
    > {
        Err(sigil_kernel::egress::DisclosurePresentationError::SinkClosed)
    }
}

#[test]
fn resource_global_cutover_boot_seam_fails_closed_then_guards_session_open() {
    use crate::application_run::ApplicationRunServices;

    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-boot",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    let run_services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter))
        .with_global_cutover(cutover);

    // Mandatory readiness: a failing probe prevents startup, no partial start.
    let error = run_services
        .require_cutover_or_fail()
        .expect_err("fail closed");
    assert!(matches!(error, CutoverErrorV1::AdapterNotReady(_)));

    // Old-schema session is explicitly unavailable for the new-epoch binary.
    let error = run_services
        .admit_session_open(StartupEpochV1::Legacy)
        .expect_err("old session unavailable");
    assert!(matches!(error, CutoverErrorV1::LegacySessionUnavailable));

    // Same-epoch open remains allowed (fixed-forward read of current-schema sessions).
    run_services
        .admit_session_open(StartupEpochV1::NewCurrentSchema)
        .expect("new epoch open");
}

#[test]
fn resource_global_cutover_sandbox_seam_readiness_is_truthful() {
    use sigil_sandbox::managed::SandboxManagedExecutionServiceV1;

    let dir = tempfile::tempdir().expect("tempdir");
    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let execution: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionServiceV1> = Arc::new(
        SandboxManagedExecutionServiceV1::new(planner, dir.path().to_path_buf()),
    );
    let storage = Arc::new(AuthorityManagedStorageServiceV1::new(
        AuthorityStorageGrantTableV1::new(),
        authority(),
    ));
    let stub_file_access = sigil_resource_authority::file_access_stub::stub_file_access_service();
    let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new(
        authority(),
        storage,
        stub_file_access,
    )
    .build_bundle();
    let registry = Arc::new(std::sync::Mutex::new(
        sigil_resource_authority::borrowed::BorrowedSubjectRegistryV1::new(),
    ));
    let file_access: Arc<dyn sigil_kernel::managed_file_access::ManagedFileAccessServiceV1> =
        Arc::new(
            sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1::new(
                registry,
            ),
        );
    let services = RuntimeManagedResourceServicesV1::compose_sandbox_backed(
        bundle,
        mock_issuer(),
        Arc::new(CutoverStubProjectionServiceV1),
        execution,
        file_access,
        RuntimeFileAccessSeamV1::AuthorityBacked,
    );
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-sandbox",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    // Execution probes now reflect the composed sandbox-backed seam.
    let probes = &cutover.manifest().mandatory_readiness;
    let one_shot = probes
        .iter()
        .find(|p| p.adapter == MandatoryAdapterKindV1::ExecutionOneShot)
        .expect("one-shot probe");
    let terminal = probes
        .iter()
        .find(|p| p.adapter == MandatoryAdapterKindV1::ExecutionTerminal)
        .expect("terminal probe");
    assert!(one_shot.passed);
    assert!(terminal.passed);
    // File access is now authority-backed: its probe passes too.
    let file_access_probe = probes
        .iter()
        .find(|p| p.adapter == MandatoryAdapterKindV1::FileAccessInProcess)
        .expect("file access probe");
    assert!(file_access_probe.passed);
    // The gate still fails closed (storage grants / desktop seams not yet cut over) and the
    // failing kind is among the not-yet-wired adapters: no partial cutover claim.
    let error = cutover.gate().expect_err("still incomplete");
    if let CutoverErrorV1::AdapterNotReady(kind) = error {
        assert_ne!(*kind, MandatoryAdapterKindV1::ExecutionOneShot);
        assert_ne!(*kind, MandatoryAdapterKindV1::ExecutionTerminal);
        assert_ne!(*kind, MandatoryAdapterKindV1::FileAccessInProcess);
    } else {
        panic!("expected AdapterNotReady, got {error:?}");
    }
}
#[test]
fn resource_global_cutover_manifest_save_and_load_round_trip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cutover-manifest.json");
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-persist",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    cutover.save_manifest(&path).expect("save");
    let loaded = RuntimeGlobalCutoverV1::load_and_validate_manifest(&path).expect("load");
    assert_eq!(loaded, *cutover.manifest());
    // Replay into the registry after restart: idempotent for the same manifest.
    let mut registry = sigil_kernel::cutover_manifest::CutoverManifestRegistryV1::new();
    registry.publish(&loaded).expect("replay");
}

#[test]
fn resource_global_cutover_manifest_tamper_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cutover-manifest.json");
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-tamper",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    cutover.save_manifest(&path).expect("save");
    // Tamper: bump the recorded generation without recomputing the content hash.
    let text = std::fs::read_to_string(&path).expect("read");
    let tampered = text.replace(
        "\"application_generation\":1",
        "\"application_generation\":9",
    );
    assert_ne!(text, tampered);
    std::fs::write(&path, tampered).expect("write");
    let error = RuntimeGlobalCutoverV1::load_and_validate_manifest(&path).expect_err("tamper");
    assert!(matches!(error, CutoverPersistenceErrorV1::CorruptManifest));
}

#[test]
fn resource_global_cutover_manifest_fixed_forward_across_boots() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cutover-manifest.json");
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let first = RuntimeGlobalCutoverV1::evaluate(
        "inst-forward",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    first.save_manifest(&path).expect("save first");
    let loaded = RuntimeGlobalCutoverV1::load_and_validate_manifest(&path).expect("load");
    let mut registry = sigil_kernel::cutover_manifest::CutoverManifestRegistryV1::new();
    registry.publish(&loaded).expect("publish");
    // A later boot with a different generation for the same instance is rejected: fixed forward.
    let second = RuntimeGlobalCutoverV1::evaluate(
        "inst-forward",
        2,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::Legacy,
    );
    let error = registry
        .publish(second.manifest())
        .expect_err("fixed forward");
    assert!(matches!(
        error,
        sigil_kernel::cutover_manifest::CutoverErrorV1::AlreadyPublished
    ));
}
#[test]
fn resource_global_cutover_legacy_decision_is_content_addressed() {
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let _ = (&services, &recovery);
    let a = RuntimeGlobalCutoverV1::legacy_decision("inst-legacy-dec", 1, authority());
    let b = RuntimeGlobalCutoverV1::legacy_decision("inst-legacy-dec", 1, authority());
    assert_eq!(a.manifest().manifest_hash, b.manifest().manifest_hash);
    assert!(a.gate().is_ok());
    assert_eq!(a.manifest().mandatory_readiness.len(), 0);
    assert_eq!(a.manifest().selected_epoch, StartupEpochV1::Legacy);
}
#[test]
fn resource_global_cutover_boot_attach_rejects_legacy_runtime() {
    use crate::application_run::ApplicationRunServices;
    let dir = tempfile::tempdir().expect("tempdir");
    let seed = dir.path().join("config.toml");
    std::fs::write(&seed, b"[core]\n").expect("seed");
    let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
    let error = attach_legacy_boot_cutover(services, &seed).expect_err("legacy rejected");
    assert!(matches!(
        error,
        CutoverBootErrorV1::Guard(CutoverErrorV1::LegacySessionUnavailable)
    ));
    let manifest_path = dir.path().join(".sigil-cutover-manifest.json");
    assert!(manifest_path.exists());
    // Reboot with the same seed remains rejected; historical data is not a runnable mode.
    let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
    let error = attach_legacy_boot_cutover(services, &seed).expect_err("legacy rejected again");
    assert!(matches!(
        error,
        CutoverBootErrorV1::Guard(CutoverErrorV1::LegacySessionUnavailable)
    ));
}

#[test]
fn resource_global_cutover_boot_attach_tampered_manifest_fails_closed() {
    use crate::application_run::ApplicationRunServices;
    let dir = tempfile::tempdir().expect("tempdir");
    let seed = dir.path().join("config.toml");
    std::fs::write(&seed, b"[core]\n").expect("seed");
    let _services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
    legacy_boot_decision(&seed).expect("write historical manifest");
    let manifest_path = dir.path().join(".sigil-cutover-manifest.json");
    // A valid-but-different manifest (drifting generation) is refused, never overwritten.
    let mut manifest =
        RuntimeGlobalCutoverV1::load_and_validate_manifest(&manifest_path).expect("load");
    assert_eq!(manifest.application_generation, 1);
    manifest.application_generation = 2;
    manifest.manifest_hash = sigil_kernel::cutover_manifest::compute_manifest_hash(&manifest);
    let bytes = serde_json::to_vec(&manifest).expect("encode");
    std::fs::write(&manifest_path, bytes).expect("write");
    let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
    let error = attach_legacy_boot_cutover(services, &seed).expect_err("drift");
    assert!(matches!(
        error,
        CutoverBootErrorV1::Guard(CutoverErrorV1::AlreadyPublished)
    ));

    // Then a tampered manifest fails closed at validation, never silently overwritten.
    let text = std::fs::read_to_string(&manifest_path).expect("read");
    let tampered = text.replace(
        "\"application_generation\":2",
        "\"application_generation\":7",
    );
    std::fs::write(&manifest_path, tampered).expect("tamper");
    let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
    let error = attach_legacy_boot_cutover(services, &seed).expect_err("tampered");
    assert!(matches!(error, CutoverBootErrorV1::Persistence(_)));
}
#[test]
fn resource_global_cutover_legacy_boot_decision_guards_sessions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let seed = dir.path().join("config.toml");
    std::fs::write(&seed, b"[core]\n").expect("seed");
    let decision = legacy_boot_decision(&seed).expect("decision");
    assert!(decision.gate().is_ok());
    assert_eq!(decision.manifest().selected_epoch, StartupEpochV1::Legacy);
    // Historical decisions are inspectable but cannot open a session.
    let error = decision
        .admit_session_open(StartupEpochV1::Legacy)
        .expect_err("legacy data is unavailable");
    assert!(matches!(error, CutoverErrorV1::LegacySessionUnavailable));
    let error = decision
        .admit_session_open(StartupEpochV1::NewCurrentSchema)
        .expect_err("legacy binary rejects new session");
    assert!(matches!(error, CutoverErrorV1::LegacyBinaryRejected));
}
#[test]
fn resource_global_cutover_guarded_session_open_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session_path = dir.path().join("session.jsonl");
    // Historical epoch data is not admitted by the current binary.
    let legacy = RuntimeGlobalCutoverV1::legacy_decision("inst-guarded", 1, authority());
    let error = guarded_session_open(&session_path, &legacy, StartupEpochV1::Legacy)
        .expect_err("legacy data unavailable");
    assert!(matches!(
        error,
        CutoverSessionOpenErrorV1::Guard(CutoverErrorV1::LegacySessionUnavailable)
    ));
    // A new-epoch binary opening the same legacy session is refused before any store open.
    let services = shadow_services(mock_issuer());
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let new_epoch = RuntimeGlobalCutoverV1::evaluate(
        "inst-new",
        1,
        authority(),
        &services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    if new_epoch.gate().is_ok() {
        // Fully cut-over surfaces reject legacy sessions through the same guard.
        let error = guarded_session_open(&session_path, &new_epoch, StartupEpochV1::Legacy)
            .expect_err("old session unavailable");
        assert!(matches!(
            error,
            CutoverSessionOpenErrorV1::Guard(CutoverErrorV1::LegacySessionUnavailable)
        ));
    } else {
        // The gate refuses to claim cutover before every adapter is wired; the guard still
        // exists and would reject the open (decision-level) even without an actual open.
        let error = new_epoch
            .admit_session_open(StartupEpochV1::Legacy)
            .expect_err("decision rejects");
        assert!(matches!(error, CutoverErrorV1::LegacySessionUnavailable));
    }
}
/// R71.6 acceptance instrument: the fully composed new-epoch surface must pass the
/// mandatory readiness gate. Red until every adapter (execution, file access, all seven
/// storage writers, extension admission, desktop borrowed/product-updater seams) is
/// composed - that is exactly the fail-closed guarantee: no partial cutover claim.
#[test]
#[ignore = "R71.6 acceptance instrument: red until every mandatory adapter is composed; enabled by --epoch current"]
fn r71_full_composition_gate() {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
    let dir = tempfile::tempdir().expect("tempdir");
    let state = dir.path().join("state");
    let exec = dir.path().join("exec");
    let config_path = dir.path().join("sigil.toml");
    let release_root = dir.path().join("release-owner");
    std::fs::write(
            &config_path,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
    std::fs::create_dir_all(&state).expect("state dir");
    std::fs::create_dir_all(state.join("cache")).expect("cache dir");
    std::fs::create_dir_all(&exec).expect("exec dir");
    std::fs::create_dir_all(&release_root).expect("release root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("mode");
        std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("mode");
    }
    let config_snapshot =
        crate::r71_authority_composition::ValidatedAuthorityConfigSnapshotV1::load(
            &config_path,
            dir.path(),
        )
        .expect("load config snapshot")
        .expect("config snapshot");
    let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
        Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
            crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
        ));
    let bootstrap =
        sigil_resource_authority::AuthorityBootstrapStoreV1::for_config_path(&config_path)
            .expect("bootstrap");
    let publication = bootstrap.acquire_publication().expect("publication");
    let process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1> =
        Arc::new(
            sigil_resource_authority::AuthorityManagedProcessInventoryV1::initialize(
                bootstrap,
                &publication,
                true,
            )
            .expect("process inventory"),
        );
    drop(publication);
    let composition =
        crate::r71_authority_composition::compose_runtime_authority_with_product_updater(
            &state,
            &state.join("cache"),
            &exec,
            &config_snapshot,
            CanonicalHash::from_bytes([0x55; 32]),
            planner,
            &[
                Ch::SessionLog,
                Ch::SessionLifecycleLog,
                Ch::InputHistory,
                Ch::DurableMemory,
                Ch::SessionCatalog,
                Ch::ArtifactStaging,
                Ch::AdapterDurableState,
            ],
            process_inventory,
        )
        .expect("compose");
    let composition = {
        let mut composition = composition;
        let release_output = std::sync::Arc::new(
            sigil_resource_authority::release_output::AuthorityBorrowedReleaseOutputServiceV1::new(
                &release_root,
            ),
        );
        composition.services = composition
            .services
            .with_optional_borrowed_release_output(Some(release_output));
        composition
    };
    let recovery = ApplicationResourceRecoveryFacadeV1::new();
    let cutover = RuntimeGlobalCutoverV1::evaluate(
        "inst-full",
        1,
        authority(),
        &composition.services,
        &recovery,
        StartupEpochV1::NewCurrentSchema,
    );
    match cutover.gate() {
        Ok(()) => {}
        Err(error) => {
            let red: Vec<_> = cutover
                .manifest()
                .mandatory_readiness
                .iter()
                .filter(|probe| !probe.passed)
                .map(|probe| format!("{:?}", probe.adapter))
                .collect();
            panic!(
                "new-epoch composition must be fully wired before gate Ok; still failing: {error:?}; red adapters: {red:?}"
            );
        }
    }
}
