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
        composition: RuntimeCompositionConfig::standard(),
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

fn ready_core_manifest() -> CutoverManifestV1 {
    let mut manifest = ready_manifest();
    manifest.composition = RuntimeCompositionConfig::core();
    manifest.mandatory_readiness = required_adapter_kinds_v1(&manifest.composition)
        .into_iter()
        .map(|adapter| probe(adapter, true))
        .collect();
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    manifest
}

#[test]
fn core_manifest_requires_authority_and_recovery_without_unselected_adapters() {
    let manifest = ready_core_manifest();
    validate_cutover_manifest(&manifest).expect("core readiness");
    assert_eq!(manifest.mandatory_readiness.len(), 13);
    assert!(CutoverSurfaceStatusV1::from_manifest(&manifest).is_ready());
    for required in [
        MandatoryAdapterKindV1::ExecutionOneShot,
        MandatoryAdapterKindV1::FileAccessInProcess,
        MandatoryAdapterKindV1::StorageSessionLog,
        MandatoryAdapterKindV1::StorageArtifact,
        MandatoryAdapterKindV1::BorrowedConfiguration,
        MandatoryAdapterKindV1::RecoverySurface,
        MandatoryAdapterKindV1::BlockingGate,
    ] {
        let mut incomplete = manifest.clone();
        incomplete
            .mandatory_readiness
            .retain(|probe| probe.adapter != required);
        incomplete.manifest_hash = compute_manifest_hash(&incomplete);
        assert_eq!(
            validate_cutover_manifest(&incomplete),
            Err(CutoverErrorV1::MissingReadinessProbe),
            "core must retain {required:?}",
        );
    }
}

#[test]
fn selected_capability_cannot_omit_its_adapter_probe() {
    for (capability, adapter) in [
        (
            OptionalCapability::Terminal,
            MandatoryAdapterKindV1::ExecutionTerminal,
        ),
        (
            OptionalCapability::Mcp,
            MandatoryAdapterKindV1::ExecutionExtension,
        ),
        (
            OptionalCapability::Skills,
            MandatoryAdapterKindV1::ExecutionExtension,
        ),
        (
            OptionalCapability::Memory,
            MandatoryAdapterKindV1::StorageMemory,
        ),
        (
            OptionalCapability::Updater,
            MandatoryAdapterKindV1::ProductStateUpdater,
        ),
    ] {
        let mut manifest = ready_core_manifest();
        manifest.composition.enhancements.insert(capability);
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        assert_eq!(
            validate_cutover_manifest(&manifest),
            Err(CutoverErrorV1::MissingReadinessProbe),
        );
        let status = CutoverSurfaceStatusV1::from_manifest(&manifest);
        assert!(
            status
                .blockers
                .iter()
                .any(|blocker| blocker.adapter == Some(adapter))
        );
    }
}

#[test]
fn composition_is_hash_bound_and_extra_or_duplicate_probes_are_rejected() {
    let mut manifest = ready_core_manifest();
    manifest
        .composition
        .enhancements
        .insert(OptionalCapability::Web);
    assert_eq!(
        validate_cutover_manifest(&manifest),
        Err(CutoverErrorV1::ManifestHashMismatch),
    );
    manifest = ready_core_manifest();
    manifest
        .mandatory_readiness
        .push(probe(MandatoryAdapterKindV1::StorageMemory, true));
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    assert_eq!(
        validate_cutover_manifest(&manifest),
        Err(CutoverErrorV1::UnexpectedReadinessProbe(
            MandatoryAdapterKindV1::StorageMemory
        )),
    );
    manifest = ready_core_manifest();
    manifest
        .mandatory_readiness
        .push(probe(MandatoryAdapterKindV1::StorageSessionLog, false));
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    assert_eq!(
        validate_cutover_manifest(&manifest),
        Err(CutoverErrorV1::DuplicateReadinessProbe(
            MandatoryAdapterKindV1::StorageSessionLog
        )),
    );
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
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    let status = CutoverSurfaceStatusV1::from_manifest(&manifest);
    assert_eq!(status.epoch, CutoverSurfaceEpochV1::NewCurrentSchema);
    assert_eq!(status.authority, CutoverAuthorityStateV1::Blocked);
    assert_eq!(status.blockers.len(), 2);
    assert!(status.blockers.iter().all(|blocker| {
        blocker.code == CutoverBlockerCodeV1::AdapterNotReady && blocker.adapter.is_some()
    }));
}

#[test]
fn cutover_surface_rejects_tampered_composition_and_probe_sets() {
    let mut changed_selection = ready_core_manifest();
    changed_selection
        .composition
        .enhancements
        .insert(OptionalCapability::Web);

    let mut changed_probe = ready_core_manifest();
    changed_probe.mandatory_readiness[0].passed = false;

    let mut unknown_schema = ready_core_manifest();
    unknown_schema.schema_version = CUTOVER_MANIFEST_SCHEMA_VERSION - 1;
    unknown_schema.manifest_hash = compute_manifest_hash(&unknown_schema);

    let mut duplicate = ready_core_manifest();
    duplicate
        .mandatory_readiness
        .push(probe(MandatoryAdapterKindV1::StorageSessionLog, false));
    duplicate.manifest_hash = compute_manifest_hash(&duplicate);

    let mut unexpected = ready_core_manifest();
    unexpected
        .mandatory_readiness
        .push(probe(MandatoryAdapterKindV1::StorageMemory, true));
    unexpected.manifest_hash = compute_manifest_hash(&unexpected);

    for manifest in [
        changed_selection,
        changed_probe,
        unknown_schema,
        duplicate,
        unexpected,
    ] {
        let status = CutoverSurfaceStatusV1::from_manifest(&manifest);
        assert_eq!(status, CutoverSurfaceStatusV1::unavailable());
        assert!(!status.is_ready());
    }
}

#[test]
fn cutover_surface_preserves_missing_core_readiness_as_a_blocker() {
    let mut manifest = ready_core_manifest();
    manifest
        .mandatory_readiness
        .retain(|probe| probe.adapter != MandatoryAdapterKindV1::BlockingGate);
    manifest.manifest_hash = compute_manifest_hash(&manifest);
    let status = CutoverSurfaceStatusV1::from_manifest(&manifest);
    assert_eq!(status.epoch, CutoverSurfaceEpochV1::NewCurrentSchema);
    assert_eq!(status.authority, CutoverAuthorityStateV1::Blocked);
    assert_eq!(
        status.blockers,
        vec![CutoverBlockerV1 {
            code: CutoverBlockerCodeV1::MissingReadinessProbe,
            adapter: Some(MandatoryAdapterKindV1::BlockingGate),
        }]
    );
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
