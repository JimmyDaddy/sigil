use super::*;

fn probe(adapter: MandatoryAdapterKindV1, passed: bool) -> AdapterReadinessProbeV1 {
    AdapterReadinessProbeV1 {
        adapter,
        passed,
        evidence_digest: CanonicalHash::from_bytes([0x11; 32]),
    }
}

fn ready_manifest() -> CutoverManifestV1 {
    let mut manifest = CutoverManifestV1 {
        schema_version: CUTOVER_MANIFEST_SCHEMA_VERSION,
        application_instance_id: "inst-1".into(),
        selected_epoch: StartupEpochV1::NewCurrentSchema,
        application_generation: 1,
        authority_generation_digest: CanonicalHash::from_bytes([0x22; 32]),
        mandatory_readiness: Vec::new(),
        manifest_hash: CanonicalHash::from_bytes([0u8; 32]),
    };
    manifest.mandatory_readiness = vec![
        MandatoryAdapterKindV1::ExecutionOneShot,
        MandatoryAdapterKindV1::ExecutionTerminal,
        MandatoryAdapterKindV1::ExecutionExtension,
        MandatoryAdapterKindV1::FileAccessInProcess,
        MandatoryAdapterKindV1::StorageSessionLog,
        MandatoryAdapterKindV1::StorageSessionLifecycle,
        MandatoryAdapterKindV1::StorageInputHistory,
        MandatoryAdapterKindV1::StorageMemory,
        MandatoryAdapterKindV1::StorageSessionCatalog,
        MandatoryAdapterKindV1::StorageArtifact,
        MandatoryAdapterKindV1::StorageAdapterDurableState,
        MandatoryAdapterKindV1::ProjectionRebuildable,
        MandatoryAdapterKindV1::ProductStateUpdater,
        MandatoryAdapterKindV1::BorrowedNativeSave,
        MandatoryAdapterKindV1::BorrowedConfiguration,
        MandatoryAdapterKindV1::BorrowedReleaseOutput,
        MandatoryAdapterKindV1::RecoverySurface,
        MandatoryAdapterKindV1::BlockingGate,
    ]
    .into_iter()
    .map(|adapter| probe(adapter, true))
    .collect();
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    manifest
}

#[test]
fn r71_cutover_new_epoch_all_adapters_ready_passes() {
    let manifest = ready_manifest();
    validate_cutover_manifest(&manifest).expect("valid new-epoch manifest");
}

#[test]
fn r71_cutover_missing_adapter_fails_closed() {
    let mut manifest = ready_manifest();
    manifest
        .mandatory_readiness
        .retain(|p| p.adapter != MandatoryAdapterKindV1::StorageArtifact);
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    let error = validate_cutover_manifest(&manifest).expect_err("missing probe");
    assert!(matches!(error, CutoverErrorV1::MissingReadinessProbe));
}

#[test]
fn r71_cutover_failed_adapter_fails_closed() {
    let mut manifest = ready_manifest();
    for probe in manifest.mandatory_readiness.iter_mut() {
        if probe.adapter == MandatoryAdapterKindV1::BlockingGate {
            probe.passed = false;
        }
    }
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    let error = validate_cutover_manifest(&manifest).expect_err("failed probe");
    assert!(matches!(
        error,
        CutoverErrorV1::AdapterNotReady(MandatoryAdapterKindV1::BlockingGate)
    ));
}

#[test]
fn r71_surface_status_marks_legacy_as_unsupported_data() {
    let mut legacy = ready_manifest();
    legacy.selected_epoch = StartupEpochV1::Legacy;
    legacy.mandatory_readiness.clear();
    legacy.manifest_hash = compute_manifest_hash(&legacy);
    let legacy_status = CutoverSurfaceStatusV1::from_manifest(&legacy);
    assert_eq!(legacy_status.epoch, CutoverSurfaceEpochV1::Legacy);
    assert_eq!(
        legacy_status.authority,
        CutoverAuthorityStateV1::Unavailable
    );
    assert_eq!(
        legacy_status.blockers[0].code,
        CutoverBlockerCodeV1::UnsupportedLegacyData
    );

    let unavailable = CutoverSurfaceStatusV1::unavailable();
    assert_eq!(unavailable.epoch, CutoverSurfaceEpochV1::Unavailable);
    assert_eq!(unavailable.authority, CutoverAuthorityStateV1::Unavailable);
    assert_eq!(
        unavailable.blockers[0].code,
        CutoverBlockerCodeV1::ManifestCorrupt
    );
}

#[test]
fn r71_surface_status_projects_all_current_schema_blockers() {
    let mut manifest = ready_manifest();
    for probe in &mut manifest.mandatory_readiness {
        if matches!(
            probe.adapter,
            MandatoryAdapterKindV1::ExecutionExtension
                | MandatoryAdapterKindV1::BorrowedConfiguration
        ) {
            probe.passed = false;
        }
    }
    let status = CutoverSurfaceStatusV1::from_manifest(&manifest);
    assert_eq!(status.epoch, CutoverSurfaceEpochV1::NewCurrentSchema);
    assert_eq!(status.authority, CutoverAuthorityStateV1::Blocked);
    assert_eq!(status.blockers.len(), 2);
    assert!(status.blockers.iter().all(|blocker| {
        blocker.code == CutoverBlockerCodeV1::AdapterNotReady && blocker.adapter.is_some()
    }));
}

#[test]
fn r71_surface_status_projects_current_schema_ready_only_when_all_probes_pass() {
    let status = CutoverSurfaceStatusV1::from_manifest(&ready_manifest());
    assert_eq!(status.authority, CutoverAuthorityStateV1::Ready);
    assert!(status.is_ready());
}

#[test]
fn r71_cutover_unknown_schema_version_fails_closed() {
    let mut manifest = ready_manifest();
    manifest.schema_version = 7;
    let error = validate_cutover_manifest(&manifest).expect_err("unknown version");
    assert!(matches!(error, CutoverErrorV1::UnknownSchemaVersion));
}

#[test]
fn r71_cutover_legacy_epoch_does_not_require_probes() {
    let mut manifest = ready_manifest();
    manifest.selected_epoch = StartupEpochV1::Legacy;
    manifest.mandatory_readiness.clear();
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    validate_cutover_manifest(&manifest).expect("legacy needs no probes");
}

#[test]
fn r71_cutover_manifest_round_trips_json_losslessly() {
    let manifest = ready_manifest();
    let encoded = serde_json::to_string(&manifest).expect("encode");
    let decoded: CutoverManifestV1 = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, manifest);
}

#[test]
fn r71_current_schema_only_new_binary_rejects_legacy_session() {
    let error = admit_session_open(SessionOpenAttemptV1 {
        session_epoch: StartupEpochV1::Legacy,
        binary_epoch: StartupEpochV1::NewCurrentSchema,
    })
    .expect_err("old session unavailable");
    assert!(matches!(error, CutoverErrorV1::LegacySessionUnavailable));
}

#[test]
fn r71_current_schema_only_legacy_binary_rejects_new_session() {
    let error = admit_session_open(SessionOpenAttemptV1 {
        session_epoch: StartupEpochV1::NewCurrentSchema,
        binary_epoch: StartupEpochV1::Legacy,
    })
    .expect_err("new session unreadable");
    assert!(matches!(error, CutoverErrorV1::LegacyBinaryRejected));
}

#[test]
fn r71_current_schema_only_matching_epoch_open_passes() {
    admit_session_open(SessionOpenAttemptV1 {
        session_epoch: StartupEpochV1::Legacy,
        binary_epoch: StartupEpochV1::Legacy,
    })
    .expect_err("legacy data is not runnable");
    admit_session_open(SessionOpenAttemptV1 {
        session_epoch: StartupEpochV1::NewCurrentSchema,
        binary_epoch: StartupEpochV1::NewCurrentSchema,
    })
    .expect("new on new");
}

#[test]
fn r71_current_schema_only_republish_identical_manifest_idempotent() {
    let manifest = ready_manifest();
    let mut registry = CutoverManifestRegistryV1::new();
    registry.publish(&manifest).expect("first publish");
    registry.publish(&manifest).expect("idempotent re-read");
}

#[test]
fn r71_current_schema_only_different_manifest_republish_rejected() {
    let mut manifest = ready_manifest();
    let mut registry = CutoverManifestRegistryV1::new();
    registry.publish(&manifest).expect("publish");
    manifest.application_generation += 1;
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    let error = registry.publish(&manifest).expect_err("fixed forward");
    assert!(matches!(error, CutoverErrorV1::AlreadyPublished));
}
