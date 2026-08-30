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
use sigil_application::ApplicationResourceRecoveryFacadeV1;

/// Actual execution seam kind the runtime surface was composed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExecutionSeamV1 {
    /// Shadow placeholder: no sandbox-backed execution protocol; probe fails closed.
    ShadowPlaceholder,
    /// Sandbox-backed managed execution protocol is composed; probe passes.
    SandboxBacked,
}

/// Actual extension (MCP / plugin) process launch seam kind: the probe never claims an
/// extension route the composition does not hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeExecutionExtensionSeamV1 {
    /// No managed extension launch route is attached; the readiness probe fails closed.
    Unavailable,
    /// Extension processes launch through the composed managed execution service; probe passes.
    ManagedExecutionBacked,
}

/// Actual desktop product/borrowed-host writer seam. A legacy value is intentionally red; the
/// probe reads this composition fact instead of claiming that a desktop writer exists merely
/// because its contract has been declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeProductStateSeamV1 {
    /// No product or borrowed-host writer is attached; the readiness probe fails closed.
    Unavailable,
    /// A trusted product-plane owner performs the bounded atomic lifecycle itself.
    ProductOwnerAtomicBacked,
    /// A borrowed-host writer is attached through the authority registration route.
    AuthorityRegistrationBacked,
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
            // Frozen matrix cell: SessionCatalog is a rebuildable database projection.
            ManagedStorageCapabilityFamilyV1::RebuildableDatabaseProjection,
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
            ManagedStorageCapabilityFamilyV1::AppendLog,
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
    recovery: &ApplicationResourceRecoveryFacadeV1,
    cutover_manifest_hash: CanonicalHash,
    application_generation: u64,
) -> Vec<AdapterReadinessProbeV1> {
    let mut out = Vec::with_capacity(18);

    let execution = matches!(
        services.execution_seam,
        RuntimeExecutionSeamV1::SandboxBacked
    );
    let extension = matches!(
        services.extension_execution_seam,
        RuntimeExecutionExtensionSeamV1::ManagedExecutionBacked
    );
    for (kind, passed) in [
        (MandatoryAdapterKindV1::ExecutionOneShot, execution),
        (MandatoryAdapterKindV1::ExecutionTerminal, execution),
        (MandatoryAdapterKindV1::ExecutionExtension, extension),
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

    for (kind, seam) in [
        (
            MandatoryAdapterKindV1::ProductStateUpdater,
            services.product_state_updater_seam,
        ),
        (
            MandatoryAdapterKindV1::BorrowedNativeSave,
            services.borrowed_native_save_seam,
        ),
        (
            MandatoryAdapterKindV1::BorrowedConfiguration,
            services.borrowed_configuration_seam,
        ),
        (
            MandatoryAdapterKindV1::BorrowedReleaseOutput,
            services.borrowed_release_output_seam,
        ),
    ] {
        let passed = matches!(
            seam,
            RuntimeProductStateSeamV1::ProductOwnerAtomicBacked
                | RuntimeProductStateSeamV1::AuthorityRegistrationBacked
        );
        out.push(AdapterReadinessProbeV1 {
            adapter: kind,
            passed,
            evidence_digest: if passed {
                CanonicalHash::from_bytes([0xb6; 32])
            } else {
                CanonicalHash::from_bytes([0xb8; 32])
            },
        });
    }

    out.push(AdapterReadinessProbeV1 {
        adapter: MandatoryAdapterKindV1::ProjectionRebuildable,
        passed: services.projection_backed,
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
    /// Builds the current-schema cutover decision for a surface. The manifest is
    /// content-addressed and the gate is evaluated immediately; the decision is immutable. Seam
    /// kinds are read from the composed surface (a probe can never claim a seam the composition
    /// does not hold).
    pub fn evaluate_current_schema(
        instance_id: impl Into<String>,
        application_generation: u64,
        authority_generation: AuthorityGeneration,
        services: &RuntimeManagedResourceServicesV1,
        recovery: &ApplicationResourceRecoveryFacadeV1,
    ) -> Self {
        let mut manifest = CutoverManifestV1 {
            schema_version: 1,
            application_instance_id: instance_id.into(),
            selected_epoch: StartupEpochV1::NewCurrentSchema,
            application_generation,
            authority_generation_digest: authority_generation.instance_hash,
            mandatory_readiness: Vec::new(),
            manifest_hash: CanonicalHash::from_bytes([0u8; 32]),
        };
        manifest.mandatory_readiness = probe_mandatory_adapters(
            services,
            recovery,
            probe_source::probe(application_generation),
            application_generation,
        );
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        let gate_error = validate_cutover_manifest(&manifest).err();
        Self {
            manifest,
            gate_ok: gate_error.is_none(),
            gate_error,
        }
    }

    #[cfg(test)]
    pub(crate) fn evaluate_for_test(
        instance_id: impl Into<String>,
        application_generation: u64,
        authority_generation: AuthorityGeneration,
        services: &RuntimeManagedResourceServicesV1,
        recovery: &ApplicationResourceRecoveryFacadeV1,
        selected_epoch: StartupEpochV1,
    ) -> Self {
        if selected_epoch == StartupEpochV1::NewCurrentSchema {
            return Self::evaluate_current_schema(
                instance_id,
                application_generation,
                authority_generation,
                services,
                recovery,
            );
        }
        let instance_id = instance_id.into();
        let mut manifest = CutoverManifestV1 {
            schema_version: 1,
            application_instance_id: instance_id,
            selected_epoch,
            application_generation,
            authority_generation_digest: authority_generation.instance_hash,
            mandatory_readiness: Vec::new(),
            manifest_hash: CanonicalHash::from_bytes([0u8; 32]),
        };
        manifest.manifest_hash = compute_manifest_hash(&manifest);
        Self {
            manifest,
            gate_ok: true,
            gate_error: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn evaluate(
        instance_id: impl Into<String>,
        application_generation: u64,
        authority_generation: AuthorityGeneration,
        services: &RuntimeManagedResourceServicesV1,
        recovery: &ApplicationResourceRecoveryFacadeV1,
        selected_epoch: StartupEpochV1,
    ) -> Self {
        Self::evaluate_for_test(
            instance_id,
            application_generation,
            authority_generation,
            services,
            recovery,
            selected_epoch,
        )
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

    /// Lossless renderer-neutral epoch/authority/blocker projection. Product surfaces must use
    /// this view rather than deriving readiness from individual seam facts.
    #[must_use]
    pub fn surface_status(&self) -> sigil_kernel::cutover_manifest::CutoverSurfaceStatusV1 {
        sigil_kernel::cutover_manifest::CutoverSurfaceStatusV1::from_manifest(&self.manifest)
    }

    /// Rehydrates an already validated current-schema manifest into the immutable runtime
    /// decision used by a worker or a later product surface. Historical legacy manifests remain
    /// inspectable as DTOs, but cannot be reintroduced as a runtime authority decision.
    #[cfg(test)]
    pub(crate) fn from_validated_manifest(
        manifest: CutoverManifestV1,
    ) -> Result<Self, CutoverErrorV1> {
        if manifest.selected_epoch != StartupEpochV1::NewCurrentSchema {
            return Err(CutoverErrorV1::AuthorityUnavailable);
        }
        validate_cutover_manifest(&manifest)?;
        let gate_error = validate_cutover_manifest(&manifest).err();
        Ok(Self {
            manifest,
            gate_ok: gate_error.is_none(),
            gate_error,
        })
    }
}

impl RuntimeGlobalCutoverV1 {
    /// Legacy-epoch decision for boot paths that have not yet cut over: by contract the legacy
    /// epoch requires no readiness probes. The manifest is content-addressed and persisted by
    /// the boot owner; the session-open guard still applies (legacy sessions only).
    #[cfg(test)]
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
    /// atomically publishes the current-instance pointer; the decision bytes remain immutable
    /// once published for that application instance.
    pub fn save_manifest(&self, path: &std::path::Path) -> Result<(), CutoverPersistenceErrorV1> {
        let bytes = serde_json::to_vec(self.manifest())
            .map_err(|error| CutoverPersistenceErrorV1::Io(error.to_string()))?;
        sigil_kernel::atomic_publish_private_file(path, &bytes)
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
        Self::validate_manifest_bytes(&bytes)
    }

    /// Validates manifest bytes that were already read through an authority-owned no-follow
    /// handle. Path permissions are intentionally checked by the caller that owns the handle.
    pub fn validate_manifest_bytes(
        bytes: &[u8],
    ) -> Result<CutoverManifestV1, CutoverPersistenceErrorV1> {
        let manifest: CutoverManifestV1 = serde_json::from_slice(bytes)
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

/// Historical test-only boot decision: selects the legacy epoch exactly once per stable instance id
/// derived from the config seed, persists the content-addressed manifest next to it and
/// replays an existing manifest instead of overwriting (tamper -> fail closed; valid but
/// drifting generation -> AlreadyPublished). Shared by every boot surface.
#[cfg(test)]
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

/// Rehydrates the published current-schema decision for a worker after the launcher has
/// completed the composition gate. A legacy manifest is never upgraded implicitly: fixed-forward
/// epoch semantics require an explicit current-schema publication first.
#[cfg(test)]
pub fn current_boot_decision(
    seed: &std::path::Path,
) -> Result<RuntimeGlobalCutoverV1, CutoverBootErrorV1> {
    let manifest_path = seed
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".sigil-cutover-manifest.json");
    let manifest = RuntimeGlobalCutoverV1::load_and_validate_manifest(&manifest_path)
        .map_err(CutoverBootErrorV1::Persistence)?;
    if manifest.selected_epoch != StartupEpochV1::NewCurrentSchema {
        return Err(CutoverBootErrorV1::Guard(CutoverErrorV1::AlreadyPublished));
    }
    RuntimeGlobalCutoverV1::from_validated_manifest(manifest).map_err(CutoverBootErrorV1::Guard)
}

/// Historical test-only boot attachment retained for compatibility fixtures. Shipping surfaces
/// use the runtime-owned current-schema boot transaction and cannot attach a legacy decision.
#[cfg(test)]
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

/// Closed guarded session-open error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CutoverSessionOpenErrorV1 {
    #[error("session epoch guard rejected the open: {0}")]
    Guard(CutoverErrorV1),
    #[error("session store open failed: {0}")]
    SessionOpen(String),
}

/// The only sanctioned session-store open in a cutover-aware surface: the decision's
/// session boundary (binary epoch vs session epoch) is checked before any store open. A
/// new-epoch binary opening a legacy session is refused here, closing the old-session
/// unavailable path end-to-end.
pub fn guarded_session_open(
    path: &std::path::Path,
    decision: &RuntimeGlobalCutoverV1,
    session_epoch: StartupEpochV1,
) -> Result<sigil_kernel::JsonlSessionStore, CutoverSessionOpenErrorV1> {
    decision
        .admit_session_open(session_epoch)
        .map_err(CutoverSessionOpenErrorV1::Guard)?;
    sigil_kernel::JsonlSessionStore::new(path)
        .map_err(|error| CutoverSessionOpenErrorV1::SessionOpen(error.to_string()))
}

#[cfg(test)]
#[path = "tests/r71_global_cutover_tests.rs"]
mod tests;
