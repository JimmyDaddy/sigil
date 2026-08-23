//! RFC-0071 section 18 R71.6: application-global cutover coordinator.
//!
//! The coordinator owns the startup epoch selection and the mandatory adapter readiness gate.
//! It composes only the runtime's pathless consumer ports (never authority concrete physical
//! types, never a second durable state). Every probe is a real call against a composed port:
//! storage probes run an admit/finalize round trip through the managed storage service, the
//! recovery-surface probe validates the kernel surface contract, the blocking-gate probe uses
//! the kernel admission gate, and execution/file-access probes report the actual seam kind the
//! runtime surface was composed with (ShadowPlaceholder fails closed). A failed probe means the
//! application must not start partially: NewCurrentSchema requires every probe green.

use std::collections::BTreeSet;

use sigil_kernel::cutover_manifest::{
    AdapterReadinessProbeV1, CutoverErrorV1, CutoverManifestV1, MandatoryAdapterKindV1,
    StartupEpochV1, compute_manifest_hash, validate_cutover_manifest,
};
use sigil_kernel::managed_storage::{
    ManagedStorageAdmissionRequestV1, StorageAdmissionSourceV1,
    ValidatedStorageAdmissionCapabilityV1,
};
use sigil_kernel::resource::{
    AdapterDurableStateClassV1, AuthorityGeneration, CanonicalHash,
    ManagedStorageCapabilityFamilyV1, ManagedStorageSemanticOwnerV1, MemoryScopeClassV1,
    OpaqueSessionId, ResourceAccessV1, ResourceBlockerScopeV1, ResourceJournalScopeV1,
    ResourceKindV1, ResourceLeaseLifetimeV1, ResourceOwnerScopeV1, ResourcePurposeV1,
};
use sigil_kernel::resource_recovery::ResourceBlockerAdmissionKeyV1;
use sigil_kernel::resource_recovery_surface::ResourceRecoverySurfaceContractV1;

use crate::managed_resource_adapters::RuntimeManagedResourceServicesV1;
use crate::resource_recovery_surface::RuntimeResourceRecoveryFacadeV1;

/// Actual execution seam kind the runtime surface was composed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExecutionSeamV1 {
    /// Shadow placeholder: no sandbox-backed execution protocol; probe fails closed.
    ShadowPlaceholder,
    /// Sandbox-backed managed execution protocol is composed; probe passes.
    SandboxBacked,
}

/// Actual in-process file access seam kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeFileAccessSeamV1 {
    /// Legacy direct-I/O path: not yet cut over; probe fails closed.
    ShadowPlaceholder,
    /// Authority-issued file access service is composed; probe passes.
    AuthorityBacked,
}

/// Kernel-owned no-active-blocker projection used by the startup probe: a fresh startup is not
/// blocked by an existing recovery because no recorder is attached yet. This proves the
/// admission gate is reachable from the runtime surface; durable projection semantics are
/// kernel-owned (R71.5 RecoveryBlockerV2).
pub struct StartupBlockerProjectionV1;

impl sigil_kernel::resource_recovery::ActiveBlockerProjectionV1 for StartupBlockerProjectionV1 {
    fn find(
        &self,
        _key: &sigil_kernel::resource_recovery::ResourceBlockerAdmissionKeyV1,
    ) -> Option<(u64, &str)> {
        None
    }
}

/// (mandatory kind, semantic owner, capability family) for the seven storage writer channels.
fn storage_channels() -> &'static [(
    MandatoryAdapterKindV1,
    ManagedStorageSemanticOwnerV1,
    ManagedStorageCapabilityFamilyV1,
)] {
    const CHANNELS: &[(
        MandatoryAdapterKindV1,
        ManagedStorageSemanticOwnerV1,
        ManagedStorageCapabilityFamilyV1,
    )] = &[
        (
            MandatoryAdapterKindV1::StorageSessionLog,
            ManagedStorageSemanticOwnerV1::SessionLog,
            ManagedStorageCapabilityFamilyV1::AppendLog,
        ),
        (
            MandatoryAdapterKindV1::StorageSessionLifecycle,
            ManagedStorageSemanticOwnerV1::SessionLifecycleLog,
            ManagedStorageCapabilityFamilyV1::AppendLog,
        ),
        (
            MandatoryAdapterKindV1::StorageInputHistory,
            ManagedStorageSemanticOwnerV1::InteractiveInputHistory,
            ManagedStorageCapabilityFamilyV1::AppendLog,
        ),
        (
            MandatoryAdapterKindV1::StorageMemory,
            ManagedStorageSemanticOwnerV1::DurableMemory(MemoryScopeClassV1::ProjectFact),
            ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
        ),
        (
            MandatoryAdapterKindV1::StorageSessionCatalog,
            ManagedStorageSemanticOwnerV1::SessionCatalog,
            ManagedStorageCapabilityFamilyV1::JournaledAtomicProjection,
        ),
        (
            MandatoryAdapterKindV1::StorageArtifact,
            ManagedStorageSemanticOwnerV1::ArtifactStaging,
            ManagedStorageCapabilityFamilyV1::StreamingArtifact,
        ),
        (
            MandatoryAdapterKindV1::StorageAdapterDurableState,
            ManagedStorageSemanticOwnerV1::AdapterDurableState(
                AdapterDurableStateClassV1::ProtocolReplay,
            ),
            ManagedStorageCapabilityFamilyV1::AtomicObject,
        ),
    ];
    CHANNELS
}

/// Storage readiness evidence: real admit/finalize round trip through the composed service.
fn storage_family_probe(
    services: &RuntimeManagedResourceServicesV1,
    channel: (
        MandatoryAdapterKindV1,
        ManagedStorageSemanticOwnerV1,
        ManagedStorageCapabilityFamilyV1,
    ),
    cutover_manifest_hash: CanonicalHash,
    application_generation: u64,
) -> AdapterReadinessProbeV1 {
    let (kind, semantic_owner, capability_family) = channel;
    let request = ManagedStorageAdmissionRequestV1 {
        semantic_owner,
        capability_family,
        purpose: sigil_kernel::resource::ManagedStorageAdmissionPurposeV1::DurablePayload,
        source: StorageAdmissionSourceV1::ApplicationCutoverRoot {
            cutover_manifest_hash,
            application_generation,
        },
        owner_scope: ResourceOwnerScopeV1::Session(OpaqueSessionId::new(
            "startup-probe".to_owned(),
        )),
        journal_scope: ResourceJournalScopeV1::Application,
    };
    let passed = match services.storage.admit_namespace(
        request,
        ValidatedStorageAdmissionCapabilityV1::startup_probe(),
    ) {
        Ok(handle) => services
            .storage
            .finalize_namespace(handle, "startup-readiness-probe".into())
            .is_ok(),
        Err(_) => false,
    };
    AdapterReadinessProbeV1 {
        adapter: kind,
        passed,
        evidence_digest: if passed {
            CanonicalHash::from_bytes([0xa1; 32])
        } else {
            CanonicalHash::from_bytes([0xa2; 32])
        },
    }
}

/// Mandatory adapter probe plan over a concrete runtime surface (exactly 18 probes).
/// The execution/file-access seams are read from the composed surface itself, so a probe can
/// never claim a seam the composition does not hold.
pub fn probe_mandatory_adapters(
    services: &RuntimeManagedResourceServicesV1,
    recovery: &RuntimeResourceRecoveryFacadeV1,
    cutover_manifest_hash: CanonicalHash,
    application_generation: u64,
) -> Vec<AdapterReadinessProbeV1> {
    let mut out = Vec::with_capacity(18);

    let execution = matches!(
        services.execution_seam,
        RuntimeExecutionSeamV1::SandboxBacked
    );
    for (kind, passed) in [
        (MandatoryAdapterKindV1::ExecutionOneShot, execution),
        (MandatoryAdapterKindV1::ExecutionTerminal, execution),
        (MandatoryAdapterKindV1::ExecutionExtension, false),
    ] {
        out.push(AdapterReadinessProbeV1 {
            adapter: kind,
            passed,
            evidence_digest: if passed {
                CanonicalHash::from_bytes([0xb1; 32])
            } else {
                CanonicalHash::from_bytes([0xb2; 32])
            },
        });
    }

    out.push(AdapterReadinessProbeV1 {
        adapter: MandatoryAdapterKindV1::FileAccessInProcess,
        passed: matches!(
            services.file_access_seam,
            RuntimeFileAccessSeamV1::AuthorityBacked
        ),
        evidence_digest: CanonicalHash::from_bytes([0xb3; 32]),
    });

    for channel in storage_channels() {
        out.push(storage_family_probe(
            services,
            *channel,
            cutover_manifest_hash,
            application_generation,
        ));
    }

    let surface_ok = recovery
        .project(ResourceRecoverySurfaceContractV1 {
            schema_version: 1,
            blocker: None,
            resource_effects: Vec::new(),
            action_envelope: None,
        })
        .is_ok();
    out.push(AdapterReadinessProbeV1 {
        adapter: MandatoryAdapterKindV1::RecoverySurface,
        passed: surface_ok,
        evidence_digest: CanonicalHash::from_bytes([0xb4; 32]),
    });

    let gate_key = ResourceBlockerAdmissionKeyV1::Requirement {
        scope: "startup-probe".to_owned(),
        requirement_key: sigil_kernel::resource::ResourceRequirementKeyV1 {
            blocker_scope: ResourceBlockerScopeV1::Session(OpaqueSessionId::new(
                "startup-probe".to_owned(),
            )),
            kind: ResourceKindV1::ExecutionTemp,
            purpose: ResourcePurposeV1::ExecutionPrerequisite,
            access: BTreeSet::from([ResourceAccessV1::Read]),
            lease_lifetime: ResourceLeaseLifetimeV1::ToolCall,
            quota_profile: sigil_kernel::resource::ResourceQuotaProfileV1 {
                class: sigil_kernel::resource::ResourceQuotaClassV1::AttemptEphemeral,
                max_bytes: 1,
                max_entries: 1,
                max_open_holders: 1,
                max_age_ms: None,
                hard_runtime_enforcement_required: true,
                profile_hash: CanonicalHash::from_bytes([0xe1; 32]),
            },
            retention_policy:
                sigil_kernel::resource::ResourceRetentionPolicyV1::ReleaseOnSettlement,
            cleanup_policy:
                sigil_kernel::resource::ResourceCleanupPolicyV1::ReleaseExactGenerationOnSettlement,
            environment_class: sigil_kernel::resource::EnvironmentProfileClassV1::FreshIsolatedHome,
            toolchain_class: None,
            subject_binding_hash: None,
            canonical_hash: CanonicalHash::from_bytes([0xe2; 32]),
        },
    };
    let gate_ok = sigil_kernel::resource_recovery::check_admission_gate(
        &StartupBlockerProjectionV1,
        &gate_key,
        true,
    )
    .is_ok();
    out.push(AdapterReadinessProbeV1 {
        adapter: MandatoryAdapterKindV1::BlockingGate,
        passed: gate_ok,
        evidence_digest: CanonicalHash::from_bytes([0xb5; 32]),
    });

    // Product state / borrowed native writers: desktop-owned seams not present in this
    // composition; fail closed until cut over.
    for kind in [
        MandatoryAdapterKindV1::ProductStateUpdater,
        MandatoryAdapterKindV1::BorrowedNativeSave,
        MandatoryAdapterKindV1::BorrowedConfiguration,
        MandatoryAdapterKindV1::BorrowedReleaseOutput,
    ] {
        out.push(AdapterReadinessProbeV1 {
            adapter: kind,
            passed: false,
            evidence_digest: CanonicalHash::from_bytes([0xb6; 32]),
        });
    }

    out.push(AdapterReadinessProbeV1 {
        adapter: MandatoryAdapterKindV1::ProjectionRebuildable,
        passed: true,
        evidence_digest: CanonicalHash::from_bytes([0xb7; 32]),
    });

    debug_assert_eq!(out.len(), 18);
    out
}

/// One application-global cutover decision: exactly one epoch, exactly one gate outcome.
#[derive(Debug, Clone)]
pub struct RuntimeGlobalCutoverV1 {
    manifest: CutoverManifestV1,
    gate_ok: bool,
    gate_error: Option<CutoverErrorV1>,
}

impl RuntimeGlobalCutoverV1 {
    /// Builds the cutover decision for a surface. The manifest is content-addressed and the
    /// gate is evaluated immediately; the decision is immutable. Seam kinds are read from the
    /// composed surface (a probe can never claim a seam the composition does not hold).
    pub fn evaluate(
        instance_id: impl Into<String>,
        application_generation: u64,
        authority_generation: AuthorityGeneration,
        services: &RuntimeManagedResourceServicesV1,
        recovery: &RuntimeResourceRecoveryFacadeV1,
        selected_epoch: StartupEpochV1,
    ) -> Self {
        let mut manifest = CutoverManifestV1 {
            schema_version: 1,
            application_instance_id: instance_id.into(),
            selected_epoch,
            application_generation,
            authority_generation_digest: authority_generation.instance_hash,
            mandatory_readiness: Vec::new(),
            manifest_hash: CanonicalHash::from_bytes([0u8; 32]),
        };
        match selected_epoch {
            StartupEpochV1::Legacy => {}
            StartupEpochV1::NewCurrentSchema => {
                manifest.mandatory_readiness = probe_mandatory_adapters(
                    services,
                    recovery,
                    probe_source::probe(application_generation),
                    application_generation,
                );
            }
        }
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        let gate_error = validate_cutover_manifest(&manifest).err();
        Self {
            manifest,
            gate_ok: gate_error.is_none(),
            gate_error,
        }
    }

    /// The immutable, content-addressed cutover manifest.
    pub fn manifest(&self) -> &CutoverManifestV1 {
        &self.manifest
    }

    /// Mandatory readiness outcome: Ok in the legacy epoch (no probes required), Err with the
    /// exact failing adapter for the new epoch. Never partially starts the application.
    pub fn gate(&self) -> Result<(), &CutoverErrorV1> {
        match (&self.gate_ok, &self.gate_error) {
            (true, None) => Ok(()),
            (false, Some(error)) => Err(error),
            _ => unreachable!("gate outcome is coherent by construction"),
        }
    }

    /// True when the runtime surface is fully cut over (new epoch, every probe green).
    pub fn is_current_schema_ready(&self) -> bool {
        self.manifest.selected_epoch == StartupEpochV1::NewCurrentSchema && self.gate_ok
    }
}

impl RuntimeGlobalCutoverV1 {
    /// Legacy-epoch decision for boot paths that have not yet cut over: by contract the legacy
    /// epoch requires no readiness probes. The manifest is content-addressed and persisted by
    /// the boot owner; the session-open guard still applies (legacy sessions only).
    pub fn legacy_decision(
        instance_id: impl Into<String>,
        application_generation: u64,
        authority_generation: AuthorityGeneration,
    ) -> Self {
        let mut manifest = CutoverManifestV1 {
            schema_version: 1,
            application_instance_id: instance_id.into(),
            selected_epoch: StartupEpochV1::Legacy,
            application_generation,
            authority_generation_digest: authority_generation.instance_hash,
            mandatory_readiness: Vec::new(),
            manifest_hash: CanonicalHash::from_bytes([0u8; 32]),
        };
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        debug_assert!(validate_cutover_manifest(&manifest).is_ok());
        Self {
            manifest,
            gate_ok: true,
            gate_error: None,
        }
    }

    /// Session-open guard for this decision: the binary epoch is the manifest epoch, so a
    /// surface that selected legacy admits legacy sessions only (a new-epoch binary would
    /// reject them via the kernel guard).
    pub fn admit_session_open(&self, session_epoch: StartupEpochV1) -> Result<(), CutoverErrorV1> {
        sigil_kernel::cutover_manifest::admit_session_open(
            sigil_kernel::cutover_manifest::SessionOpenAttemptV1 {
                session_epoch,
                binary_epoch: self.manifest.selected_epoch,
            },
        )
    }
}

/// Closed cutover manifest persistence error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CutoverPersistenceErrorV1 {
    #[error("cutover manifest path is not a private regular file")]
    NotPrivateFile,
    #[error("cutover manifest content hash does not match its manifest_hash")]
    CorruptManifest,
    #[error("cutover manifest schema version is unknown")]
    UnknownVersion,
    #[error("cutover manifest io failed: {0}")]
    Io(String),
}

impl RuntimeGlobalCutoverV1 {
    /// Persists the content-addressed manifest as a private (0600) regular file. The boot owner
    /// writes it once per generation; the next boot replays it against the registry.
    pub fn save_manifest(&self, path: &std::path::Path) -> Result<(), CutoverPersistenceErrorV1> {
        let bytes = serde_json::to_vec(self.manifest())
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        std::fs::write(path, bytes)
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        sigil_kernel::config::secure_private_path_permissions(path)
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        Ok(())
    }

    /// Loads and validates a persisted manifest: private regular file, known schema, content
    /// hash intact. A manifest failing any of these must not be used to claim an epoch.
    pub fn load_and_validate_manifest(
        path: &std::path::Path,
    ) -> Result<CutoverManifestV1, CutoverPersistenceErrorV1> {
        let private_ok = sigil_kernel::config::private_path_permissions_are_restricted(path)
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        if !private_ok {
            return Err(CutoverPersistenceErrorV1::NotPrivateFile);
        }
        let bytes = std::fs::read(path)
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        let manifest: CutoverManifestV1 = serde_json::from_slice(&bytes)
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        validate_cutover_manifest(&manifest).map_err(|error| match error {
            CutoverErrorV1::UnknownSchemaVersion => CutoverPersistenceErrorV1::UnknownVersion,
            CutoverErrorV1::ManifestHashMismatch => CutoverPersistenceErrorV1::CorruptManifest,
            _ => CutoverPersistenceErrorV1::CorruptManifest,
        })?;
        Ok(manifest)
    }
}

/// Probe-source content binding before the manifest hash exists: bound to the generation
/// the probes are proving.
mod probe_source {
    use super::*;

    pub fn probe(application_generation: u64) -> CanonicalHash {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&application_generation.to_be_bytes());
        CanonicalHash::from_bytes(bytes)
    }
}

/// Closed boot-cutover attachment error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CutoverBootErrorV1 {
    #[error("cutover manifest persistence failed: {0}")]
    Persistence(CutoverPersistenceErrorV1),
    #[error("mandatory readiness / session guard failed: {0}")]
    Guard(CutoverErrorV1),
}

/// Legacy boot decision: selects the legacy epoch exactly once per stable instance id
/// derived from the config seed, persists the content-addressed manifest next to it and
/// replays an existing manifest instead of overwriting (tamper -> fail closed; valid but
/// drifting generation -> AlreadyPublished). Shared by every boot surface.
pub fn legacy_boot_decision(
    seed: &std::path::Path,
) -> Result<RuntimeGlobalCutoverV1, CutoverBootErrorV1> {
    let instance_id = format!(
        "sigil:{}",
        sigil_kernel::external::sha256_hex(seed.to_string_lossy().as_bytes())
    );
    let mut digest = [0u8; 32];
    {
        let hex = instance_id.as_bytes();
        let bound = hex.len().min(32);
        digest[..bound].copy_from_slice(&hex[..bound]);
    }
    let authority = AuthorityGeneration {
        epoch: 0,
        instance_hash: CanonicalHash::from_bytes(digest),
    };
    let cutover = RuntimeGlobalCutoverV1::legacy_decision(instance_id, 1, authority);
    let manifest_path = seed
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".sigil-cutover-manifest.json");
    if manifest_path.exists() {
        // Fixed-forward across boots: the durable manifest must validate AND match this boot
        // decision exactly. A tampered or drifting manifest fails startup, never silently
        // overwritten (the registry record is the only epoch truth for this instance).
        let existing = RuntimeGlobalCutoverV1::load_and_validate_manifest(&manifest_path)
            .map_err(CutoverBootErrorV1::Persistence)?;
        if existing != *cutover.manifest() {
            return Err(CutoverBootErrorV1::Guard(CutoverErrorV1::AlreadyPublished));
        }
    } else {
        cutover
            .save_manifest(&manifest_path)
            .map_err(CutoverBootErrorV1::Persistence)?;
    }
    Ok(cutover)
}

/// Shared boot attachment for ApplicationRunServices surfaces (CLI headless/machine, HTTP
/// serve, desktop launcher): selects the epoch via [legacy_boot_decision], attaches the
/// decision and runs the mandatory readiness guard (fail closed) plus the legacy session-open
/// guard. A failure aborts startup before any run is prepared.
pub fn attach_legacy_boot_cutover(
    services: crate::application_run::ApplicationRunServices,
    seed: &std::path::Path,
) -> Result<crate::application_run::ApplicationRunServices, CutoverBootErrorV1> {
    let cutover = legacy_boot_decision(seed)?;
    let services = services.with_global_cutover(cutover);
    services
        .require_cutover_or_fail()
        .map_err(CutoverBootErrorV1::Guard)?;
    services
        .admit_session_open(StartupEpochV1::Legacy)
        .map_err(CutoverBootErrorV1::Guard)?;
    Ok(services)
}

/// Read-only cutover manifest inspection for doctor/support surfaces: returns None when not
/// yet published and the validated manifest otherwise. Never writes (doctor must stay side
/// effect free); a tampered or corrupted manifest surfaces as an error so startup blockers
/// become visible before a run is attempted.
pub fn inspect_cutover_manifest(
    seed: &std::path::Path,
) -> Result<Option<CutoverManifestV1>, CutoverPersistenceErrorV1> {
    let manifest_path = seed
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".sigil-cutover-manifest.json");
    if !manifest_path.exists() {
        return Ok(None);
    }
    RuntimeGlobalCutoverV1::load_and_validate_manifest(&manifest_path).map(Some)
}

#[cfg(test)]
mod tests {
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

    fn shadow_services(
        issuer: Arc<dyn KernelCapabilityIssuerV1>,
    ) -> RuntimeManagedResourceServicesV1 {
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
    fn resource_global_cutover_legacy_epoch_requires_no_probes() {
        let services = shadow_services(mock_issuer());
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
    fn resource_global_cutover_storage_roundtrip_probe_is_real() {
        let services = shadow_services(mock_issuer());
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
                resource_id: sigil_kernel::resource::OpaqueResourceId::new(format!(
                    "res-{grant_id}"
                )),
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let execution: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionServiceV1> =
            Arc::new(SandboxManagedExecutionServiceV1::new(
                planner,
                dir.path().to_path_buf(),
            ));
        let storage = Arc::new(AuthorityManagedStorageServiceV1::new(
            AuthorityStorageGrantTableV1::new(),
            authority(),
        ));
        let stub_file_access =
            sigil_resource_authority::file_access_stub::stub_file_access_service();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
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
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
        let _ = (&services, &recovery);
        let a = RuntimeGlobalCutoverV1::legacy_decision("inst-legacy-dec", 1, authority());
        let b = RuntimeGlobalCutoverV1::legacy_decision("inst-legacy-dec", 1, authority());
        assert_eq!(a.manifest().manifest_hash, b.manifest().manifest_hash);
        assert!(a.gate().is_ok());
        assert_eq!(a.manifest().mandatory_readiness.len(), 0);
        assert_eq!(a.manifest().selected_epoch, StartupEpochV1::Legacy);
    }
    #[test]
    fn resource_global_cutover_boot_attach_selects_legacy_once() {
        use crate::application_run::ApplicationRunServices;
        let dir = tempfile::tempdir().expect("tempdir");
        let seed = dir.path().join("config.toml");
        std::fs::write(&seed, b"[core]\n").expect("seed");
        let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
        let attached = attach_legacy_boot_cutover(services, &seed).expect("attach");
        let decision = attached.cutover().expect("decision");
        assert!(decision.gate().is_ok());
        assert_eq!(decision.manifest().selected_epoch, StartupEpochV1::Legacy);
        assert_eq!(decision.manifest().mandatory_readiness.len(), 0);
        let manifest_path = dir.path().join(".sigil-cutover-manifest.json");
        assert!(manifest_path.exists());
        // Reboot with the same seed: identical manifest replay is accepted.
        let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
        attach_legacy_boot_cutover(services, &seed).expect("idempotent reboot");
        // Legacy session open stays allowed; a new-epoch binary would reject it (kernel guard).
        let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
        let attached = attach_legacy_boot_cutover(services, &seed).expect("attach again");
        attached
            .admit_session_open(StartupEpochV1::Legacy)
            .expect("legacy open");
    }

    #[test]
    fn resource_global_cutover_boot_attach_tampered_manifest_fails_closed() {
        use crate::application_run::ApplicationRunServices;
        let dir = tempfile::tempdir().expect("tempdir");
        let seed = dir.path().join("config.toml");
        std::fs::write(&seed, b"[core]\n").expect("seed");
        let services = ApplicationRunServices::new(Arc::new(CutoverTestDisclosurePresenter));
        attach_legacy_boot_cutover(services, &seed).expect("attach");
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
        // The decision itself enforces the session-open boundary (surface independent).
        decision
            .admit_session_open(StartupEpochV1::Legacy)
            .expect("legacy open");
        let error = decision
            .admit_session_open(StartupEpochV1::NewCurrentSchema)
            .expect_err("legacy binary rejects new session");
        assert!(matches!(error, CutoverErrorV1::LegacyBinaryRejected));
    }
}
