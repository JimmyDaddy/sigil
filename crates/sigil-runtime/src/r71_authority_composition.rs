//! RFC-0071 section 18 R71.6: production authority composition spine.
//!
//! The only place a boot surface turns verified bootstrap anchors + declared writer channels
//! into the composed runtime surface (services, storage writer adapter, authority-backed file
//! access). Declaring a writer channel registers exactly its grant: the cutover probe then
//! reflects what is composed and nothing more. Real authority services only - no stub in the
//! production path (the capability issuer, planner and projection facade are host-injected
//! because their production construction belongs to kernel/boot owners).

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use sigil_kernel::capability_issuer::KernelCapabilityIssuerV1;
use sigil_kernel::managed_execution::ManagedExecutionPlannerV1;
use sigil_kernel::managed_file_access::ManagedFileAccessServiceV1;
use sigil_kernel::managed_projection::ManagedProjectionServiceV1;
use sigil_kernel::managed_storage::ManagedStorageServiceV1;
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash, ResourceJournalScopeV1};

use crate::managed_resource_adapters::RuntimeManagedExtensionExecutionRouteV1;
use crate::managed_resource_adapters::RuntimeManagedResourceServicesV1;
use crate::managed_storage_writer::{
    ManagedStorageWriterAdapterV1, StorageWriterChannelV1, grant_for_channel,
};

/// Composed runtime authority surface (everything a new-epoch boot needs once).
pub struct RuntimeAuthorityCompositionV1 {
    pub services: RuntimeManagedResourceServicesV1,
    pub storage_writer: std::sync::Arc<ManagedStorageWriterAdapterV1>,
    pub declared_channels: BTreeSet<StorageWriterChannelV1>,
    /// The composition's single real capability broker: surfaces seal/issue through this
    /// (one-shot proofs; kernel-side binding).
    pub broker: std::sync::Arc<sigil_kernel::capability_issuer::KernelCapabilityBrokerV1>,
    /// Kernel-owned tool authority facade: in-process file tools seal -> issue -> adjudicate
    /// through this (one-shot tool tokens; never fabricated by a tool).
    pub tool_authority: sigil_kernel::tool_authority::KernelToolAuthorityV1,
    /// Managed Extension route used by eager/lazy MCP stdio activation.
    pub extension_execution: std::sync::Arc<RuntimeManagedExtensionExecutionRouteV1>,
}

impl std::fmt::Debug for RuntimeAuthorityCompositionV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeAuthorityCompositionV1")
            .field("declared_channels", &self.declared_channels)
            .field("projection_backed", &self.services.projection_backed)
            .finish_non_exhaustive()
    }
}

/// Closed composition error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeAuthorityCompositionErrorV1 {
    #[error("bootstrap anchor validation failed: {0}")]
    AnchorInvalid(String),
    #[error("declared writer grant failed: {0}")]
    GrantDeclared(String),
}

/// Composes the R71.6 authority surface from verified anchors and declared channels.
///
/// `state_anchor` / `execution_temp_root` must already exist as owner-only dirs (the boot
/// owner resolves them through the authority bootstrap resolver; this function re-validates
/// via [validate anchors]). `planner` and `issuer` are host-injected (kernel/boot owned); the
/// projection facade is the runtime transitional edge.
#[allow(clippy::too_many_arguments)]
pub fn compose_runtime_authority(
    state_anchor: &Path,
    execution_temp_root: &Path,
    cutover_manifest_hash: CanonicalHash,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    // The real kernel capability broker is the single issuer for this composition: execution
    // bundles and storage admission capabilities are broker-issued (one-shot proofs), never
    // fabricated by consumers.
    let broker =
        std::sync::Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new());
    let bootstrap = sigil_resource_authority::bootstrap::AuthorityBootstrapRoots {
        state_anchor: state_anchor.to_path_buf(),
        cache_anchor: state_anchor.join("cache"),
        execution_temp_anchor: execution_temp_root.to_path_buf(),
        state_identity: CanonicalHash::from_bytes([0x71; 32]),
        cache_identity: CanonicalHash::from_bytes([0x72; 32]),
        execution_temp_identity: CanonicalHash::from_bytes([0x73; 32]),
        manifest_hash: cutover_manifest_hash,
        journal_instance_hash: CanonicalHash::from_bytes([0x74; 32]),
    };
    bootstrap
        .validate_anchors()
        .map_err(|error| RuntimeAuthorityCompositionErrorV1::AnchorInvalid(error.to_string()))?;

    let authority = AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x75; 32]),
    };
    let mut table = sigil_resource_authority::storage::AuthorityStorageGrantTableV1::new();
    for channel in declared {
        if *channel == crate::managed_storage_writer::StorageWriterChannelV1::DurableMemory {
            // Both DurableMemory scope classes are writer channels of the same family: the
            // ProjectFact grant covers the probe cell; the UserPreference grant keeps the
            // two-class DurableMemory writer from being partially closed.
            for grant in crate::managed_storage_writer::memory_grants(0x76) {
                table.register(grant).map_err(|error| {
                    RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
                })?;
            }
            continue;
        }
        let grant = grant_for_channel(*channel, 0x76);
        table.register(grant).map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
        })?;
    }
    let storage: Arc<dyn ManagedStorageServiceV1> = Arc::new(
        sigil_resource_authority::storage::AuthorityManagedStorageServiceV1::new(table, authority),
    );
    let registry = Arc::new(std::sync::Mutex::new(
        sigil_resource_authority::borrowed::BorrowedSubjectRegistryV1::new(),
    ));
    let file_access: Arc<dyn ManagedFileAccessServiceV1> = Arc::new(
        sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1::new(registry),
    );
    let execution: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionServiceV1> = Arc::new(
        sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&planner),
            execution_temp_root.to_path_buf(),
        ),
    );
    let extension_execution = std::sync::Arc::new(RuntimeManagedExtensionExecutionRouteV1::new(
        Arc::clone(&planner),
        Arc::clone(&broker),
        execution_temp_root.to_path_buf(),
    ));
    let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new(
        authority,
        storage.clone() as Arc<dyn ManagedStorageServiceV1>,
        file_access.clone() as Arc<dyn ManagedFileAccessServiceV1>,
    )
    .build_bundle();
    let records_projection = std::sync::Arc::new(
        crate::runtime_records_projection::RuntimeRecordsProjectionServiceV1::new(
            state_anchor.to_path_buf(),
        ),
    );
    let services =
        RuntimeManagedResourceServicesV1::compose_sandbox_backed_with_extension_execution(
            bundle,
            broker.clone() as Arc<dyn KernelCapabilityIssuerV1>,
            records_projection as Arc<dyn ManagedProjectionServiceV1>,
            execution,
            Arc::clone(&file_access),
            crate::r71_global_cutover::RuntimeFileAccessSeamV1::AuthorityBacked,
            Arc::clone(&extension_execution),
        );
    let storage_writer = std::sync::Arc::new(ManagedStorageWriterAdapterV1::with_storage_issuer(
        storage,
        state_anchor.to_path_buf(),
        cutover_manifest_hash,
        std::sync::Arc::clone(&broker),
    ));
    Ok(RuntimeAuthorityCompositionV1 {
        services,
        storage_writer,
        declared_channels: declared.iter().copied().collect(),
        broker: std::sync::Arc::clone(&broker),
        tool_authority: sigil_kernel::tool_authority::KernelToolAuthorityV1::new(
            Arc::clone(&file_access),
            Arc::clone(&broker),
        ),
        extension_execution,
    })
}

/// Convenience: authoritative resource journal scope for the composition (application-level).
pub fn composition_journal_scope() -> ResourceJournalScopeV1 {
    ResourceJournalScopeV1::Application
}
/// Closed boot-authority attach error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BootAuthorityErrorV1 {
    #[error("config load failed: {0}")]
    Config(String),
    #[error("cutover attach failed: {0}")]
    Cutover(crate::r71_global_cutover::CutoverBootErrorV1),
    #[error("authority composition failed: {0}")]
    Composition(RuntimeAuthorityCompositionErrorV1),
}

/// One-call boot attach shared by CLI headless/machine and HTTP serve: selects the legacy
/// epoch (durable manifest, fixed-forward), prepares the authority anchors and composes the
/// authority surface once, then attaches both to the run services (fail closed on any step).
pub fn attach_boot_authority_to_services(
    services: crate::application_run::ApplicationRunServices,
    config_path: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Result<crate::application_run::ApplicationRunServices, BootAuthorityErrorV1> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
    // Config may be absent on first run or non-regular (streamed FIFO): attach the epoch only
    // (manifest + guard) and skip authority composition, which requires a real config file.
    let config_meta = match std::fs::symlink_metadata(config_path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return crate::r71_global_cutover::attach_legacy_boot_cutover(services, config_path)
                .map_err(BootAuthorityErrorV1::Cutover);
        }
        Err(error) => {
            return Err(BootAuthorityErrorV1::Config(error.to_string()));
        }
    };
    if !config_meta.file_type().is_file() {
        return crate::r71_global_cutover::attach_legacy_boot_cutover(services, config_path)
            .map_err(BootAuthorityErrorV1::Cutover);
    }
    // A malformed config must not hard-block the recovery/first-run server: degrade to an
    // epoch-only attach (manifest + guard, no authority composition) exactly like absent
    // configs. The authority gate applies to runs with a valid config.
    let root_config = match sigil_kernel::RootConfig::load(config_path) {
        Ok(config) => config,
        Err(_) => {
            return crate::r71_global_cutover::attach_legacy_boot_cutover(services, config_path)
                .map_err(BootAuthorityErrorV1::Cutover);
        }
    };
    let cutover = crate::r71_global_cutover::legacy_boot_decision(config_path)
        .map_err(BootAuthorityErrorV1::Cutover)?;
    let paths =
        crate::resolve_sigil_paths(&root_config.storage, &root_config.session, workspace_root);
    for anchor in [&paths.state_root, &paths.scratch_root] {
        std::fs::create_dir_all(anchor)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(anchor, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        }
    }
    std::fs::create_dir_all(paths.state_root.join("cache"))
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            paths.state_root.join("cache"),
            std::fs::Permissions::from_mode(0o700),
        )
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    }
    let planner = std::sync::Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
        crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
    ));
    let composition = compose_runtime_authority(
        &paths.state_root,
        &paths.scratch_root,
        cutover.manifest().manifest_hash,
        planner,
        &[Ch::SessionLog, Ch::InputHistory, Ch::SessionCatalog],
    )
    .map_err(BootAuthorityErrorV1::Composition)?;
    let services = crate::r71_global_cutover::attach_legacy_boot_cutover(services, config_path)
        .map_err(BootAuthorityErrorV1::Cutover)?;
    Ok(services.with_authority_composition(composition))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r71_global_cutover::{
        RuntimeExecutionSeamV1, RuntimeFileAccessSeamV1, probe_mandatory_adapters,
    };
    use crate::resource_recovery_surface::RuntimeResourceRecoveryFacadeV1;
    use sigil_kernel::cutover_manifest::MandatoryAdapterKindV1;

    #[test]
    fn r71_composition_tool_authority_facade_is_wired() {
        use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        let exec = dir.path().join("exec");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::create_dir_all(state.join("cache")).expect("cache dir");
        std::fs::create_dir_all(&exec).expect("exec dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("mode");
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let composition = compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x55; 32]),
            planner,
            &[Ch::SessionLog],
        )
        .expect("compose");
        let binding =
            sigil_kernel::managed_file_access::ManagedFileAdmissionBindingV1::ToolPermissionPlan {
                permission_plan_hash: CanonicalHash::from_bytes([0xa1; 32]),
                decision_hash: CanonicalHash::from_bytes([0xa2; 32]),
                approval_continuity_hash: CanonicalHash::from_bytes([0xa3; 32]),
                tool_start_event_digest: CanonicalHash::from_bytes([0xa4; 32]),
                file_access_plan_hash: CanonicalHash::from_bytes([0xa5; 32]),
                file_subject_binding_hash: CanonicalHash::from_bytes([0xa6; 32]),
                file_resolver_proof_digest: CanonicalHash::from_bytes([0xa7; 32]),
                file_authority_generation: sigil_kernel::resource::AuthorityGeneration {
                    epoch: 1,
                    instance_hash: CanonicalHash::from_bytes([0xa8; 32]),
                },
                workspace_mutation_activation: None,
            };
        let subject = sigil_kernel::resource::OpaquePermissionSubjectRef::new("ws-1".to_owned());
        let outcome = composition.tool_authority.adjudicate_tool_file_access(
            binding,
            &subject,
            sigil_kernel::managed_file_access::ManagedFileOperationV1::Read,
        );
        // Observable subjects are registered by the surface bootstrap; an unobserved subject
        // fails closed through the real wire.
        let error = outcome.expect_err("unregistered subject");
        assert!(matches!(
            error,
            sigil_kernel::tool_authority::KernelToolAuthorityErrorV1::Access(
                sigil_kernel::managed_file_access::ManagedFileAccessErrorV1::OperationNotPermitted
            )
        ));
    }

    #[test]
    fn r71_composition_declared_channel_writes_and_probes_exactly() {
        use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        let exec = dir.path().join("exec");
        std::fs::create_dir_all(&state).expect("state dir");
        std::fs::create_dir_all(state.join("cache")).expect("cache dir");
        std::fs::create_dir_all(&exec).expect("exec dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).expect("mode");
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700)).expect("mode");
        }
        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let composition = crate::r71_authority_composition::compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x55; 32]),
            planner,
            &[Ch::SessionLog],
        )
        .expect("compose");
        let lease = composition
            .storage_writer
            .acquire(Ch::SessionLog)
            .expect("acquire");
        composition
            .storage_writer
            .write_record(&lease, b"seq=1")
            .expect("write");
        composition
            .storage_writer
            .finalize(lease)
            .expect("finalize");
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
        let probes = probe_mandatory_adapters(
            &composition.services,
            &recovery,
            CanonicalHash::from_bytes([0x56; 32]),
            1,
        );
        let session_log = probes
            .iter()
            .find(|p| p.adapter == MandatoryAdapterKindV1::StorageSessionLog)
            .expect("session log probe");
        assert!(session_log.passed);
        let input_history = probes
            .iter()
            .find(|p| p.adapter == MandatoryAdapterKindV1::StorageInputHistory)
            .expect("input history probe");
        assert!(!input_history.passed);
        assert!(matches!(
            composition.services.execution_seam,
            RuntimeExecutionSeamV1::SandboxBacked
        ));
        assert!(matches!(
            composition.services.file_access_seam,
            RuntimeFileAccessSeamV1::AuthorityBacked
        ));
    }
}
