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

use crate::managed_resource_adapters::RuntimeManagedResourceServicesV1;
use crate::managed_resource_adapters::{
    RuntimeManagedCommandExecutionRouteV1, RuntimeManagedExtensionExecutionRouteV1,
    RuntimeManagedPluginHookExecutionRouteV1,
};
use crate::managed_storage_writer::{ManagedStorageWriterAdapterV1, StorageWriterChannelV1};

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
    /// Extension-purpose route used exclusively by trusted plugin hook execution.
    pub plugin_hook_execution: std::sync::Arc<RuntimeManagedPluginHookExecutionRouteV1>,
    /// Managed one-shot command route used by the built-in Bash surface.
    pub command_execution: std::sync::Arc<RuntimeManagedCommandExecutionRouteV1>,
    authority_generation: AuthorityGeneration,
    borrowed_workspace_registry: std::sync::Arc<
        std::sync::Mutex<sigil_resource_authority::borrowed::BorrowedSubjectRegistryV1>,
    >,
}

impl RuntimeAuthorityCompositionV1 {
    /// Creates a hook runner already bound to this composition's Extension-purpose authority.
    #[must_use]
    pub fn plugin_hook_runner(&self) -> crate::plugins::PluginHookExecutionRunner {
        let route: Arc<dyn crate::plugins::ManagedPluginHookExecutionPortV1> =
            self.plugin_hook_execution.clone();
        crate::plugins::PluginHookExecutionRunner::new(route)
    }

    /// Returns the only production child-resource provisioner. It closes the child over the
    /// composed writer/artifact authority and kernel tool authority; no parent raw store or
    /// writer token is handed to plan-review code.
    pub fn plan_review_child_resource_provisioner(
        &self,
    ) -> std::sync::Arc<dyn crate::plan_review_coordinator::PlanReviewChildResourceProvisionerV1>
    {
        std::sync::Arc::new(
            crate::plan_review_coordinator::RuntimePlanReviewChildResourceProvisionerV1::new_with_generation(
                std::sync::Arc::clone(&self.storage_writer),
                std::sync::Arc::new(self.tool_authority.clone()),
                self.authority_generation,
            ),
        )
    }

    /// Exact generation bound into every current-schema child bundle.
    #[must_use]
    pub const fn authority_generation(&self) -> AuthorityGeneration {
        self.authority_generation
    }

    /// Registers the exact workspace root used by this composition. Builtin file tools may not
    /// plan or execute before this activation succeeds.
    pub fn activate_workspace(
        &self,
        workspace_root: &Path,
    ) -> Result<sigil_resource_authority::borrowed::BorrowedWorkspaceRegistrationCapsuleV1, String>
    {
        let workspace_id =
            sigil_kernel::stable_workspace_id(workspace_root).map_err(|error| error.to_string())?;
        self.borrowed_workspace_registry
            .lock()
            .map_err(|_| "borrowed workspace registry is poisoned".to_owned())?
            .activate_workspace(
                "sigil",
                workspace_id.as_str().to_owned(),
                workspace_root,
                AuthorityGeneration {
                    epoch: 1,
                    instance_hash: CanonicalHash::from_bytes([0x75; 32]),
                },
            )
            .map_err(|error| error.to_string())
    }
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
    #[error("durable authority journal failed: {0}")]
    JournalUnavailable(String),
    #[error("execution configuration failed: {0}")]
    ExecutionConfigurationInvalid(String),
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
    compose_runtime_authority_inner(
        state_anchor,
        execution_temp_root,
        cutover_manifest_hash,
        planner,
        declared,
        None,
        sigil_kernel::ExecutionConfig::default(),
    )
}

/// Composes the production authority surface with the independent product updater owner.
///
/// The updater cache is a trusted product-plane object, not a managed agent resource. Keeping
/// its owner explicit here lets the cutover probe verify the real writer attachment without
/// granting the updater an agent/session capability.
#[allow(clippy::too_many_arguments)]
pub fn compose_runtime_authority_with_product_updater(
    state_anchor: &Path,
    cache_root: &Path,
    execution_temp_root: &Path,
    config_path: &Path,
    cutover_manifest_hash: CanonicalHash,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    let execution_config = sigil_kernel::RootConfig::load(config_path)
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::ExecutionConfigurationInvalid(error.to_string())
        })?
        .execution;
    let mut composition = compose_runtime_authority_inner(
        state_anchor,
        execution_temp_root,
        cutover_manifest_hash,
        planner,
        declared,
        Some(Arc::new(
            sigil_updater::ProductUpdaterState::from_cache_root(cache_root),
        )),
        execution_config,
    )?;
    let configuration_service: Arc<
        dyn sigil_resource_authority::configuration::BorrowedConfigurationServiceV1,
    > = Arc::new(
        sigil_resource_authority::configuration::AuthorityBorrowedConfigurationServiceV1::new(
            config_path,
        ),
    );
    let release_output: Arc<
        dyn sigil_resource_authority::release_output::BorrowedReleaseOutputServiceV1,
    > = Arc::new(
        sigil_resource_authority::release_output::AuthorityBorrowedReleaseOutputServiceV1::new(
            state_anchor.join("release-output"),
        ),
    );
    composition.services = composition
        .services
        .with_optional_borrowed_configuration(Some(configuration_service))
        .with_optional_borrowed_release_output(Some(release_output));
    Ok(composition)
}

#[allow(clippy::too_many_arguments)]
fn compose_runtime_authority_inner(
    state_anchor: &Path,
    execution_temp_root: &Path,
    cutover_manifest_hash: CanonicalHash,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
    product_updater: Option<Arc<sigil_updater::ProductUpdaterState>>,
    execution_config: sigil_kernel::ExecutionConfig,
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    // The real kernel capability broker is the single issuer for this composition: execution
    // bundles and storage admission capabilities are broker-issued (one-shot proofs), never
    // fabricated by consumers.
    let journal_instance_hash = hash_path_binding("authority-journal-instance-v1", state_anchor);
    let bootstrap_manifest_hash =
        hash_path_binding("authority-bootstrap-manifest-v1", state_anchor);
    let broker =
        std::sync::Arc::new(sigil_kernel::capability_issuer::KernelCapabilityBrokerV1::new());
    let bootstrap = sigil_resource_authority::bootstrap::AuthorityBootstrapRoots {
        state_anchor: state_anchor.to_path_buf(),
        cache_anchor: state_anchor.join("cache"),
        execution_temp_anchor: execution_temp_root.to_path_buf(),
        state_identity: CanonicalHash::from_bytes([0x71; 32]),
        cache_identity: CanonicalHash::from_bytes([0x72; 32]),
        execution_temp_identity: CanonicalHash::from_bytes([0x73; 32]),
        manifest_hash: bootstrap_manifest_hash,
        journal_instance_hash,
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
            for grant in crate::managed_storage_writer::memory_grants_with_context(
                0x76,
                authority,
                cutover_manifest_hash,
            ) {
                table.register(grant).map_err(|error| {
                    RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
                })?;
            }
            continue;
        }
        let grant = crate::managed_storage_writer::grant_for_channel_with_context(
            *channel,
            0x76,
            authority,
            cutover_manifest_hash,
        );
        table.register(grant).map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
        })?;
    }
    let storage_service =
        sigil_resource_authority::storage::AuthorityManagedStorageServiceV1::new_with_journal(
            table,
            authority,
            state_anchor.join("authority-resources.journal.json"),
            bootstrap_manifest_hash,
            journal_instance_hash,
        )
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::JournalUnavailable(error.to_string())
        })?;
    storage_service
        .reconcile_unsettled_storage_grants_with_physical_bridge()
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::JournalUnavailable(error.to_string())
        })?;
    storage_service
        .require_startup_reconciliation()
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::JournalUnavailable(error.to_string())
        })?;
    let storage: Arc<dyn ManagedStorageServiceV1> = Arc::new(storage_service);
    let registry = Arc::new(std::sync::Mutex::new(
        sigil_resource_authority::borrowed::BorrowedSubjectRegistryV1::new(),
    ));
    let file_access_impl = Arc::new(
        sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1::new(
            Arc::clone(&registry),
        ),
    );
    let file_access: Arc<dyn ManagedFileAccessServiceV1> = file_access_impl.clone();
    let borrowed_native_save: Arc<
        dyn sigil_resource_authority::native_save::BorrowedNativeSaveServiceV1,
    > = Arc::new(
        sigil_resource_authority::native_save::AuthorityBorrowedNativeSaveServiceV1::new(
            // The file-access adapter and native-save adapter observe through one authority
            // registry, so a registration capsule cannot bypass the existing subject table.
            Arc::clone(&registry),
        ),
    );
    let execution: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionServiceV1> = Arc::new(
        sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
            Arc::clone(&planner),
            execution_temp_root.to_path_buf(),
        ),
    );
    let extension_execution = std::sync::Arc::new(
        RuntimeManagedExtensionExecutionRouteV1::new(
            Arc::clone(&planner),
            Arc::clone(&broker),
            execution_temp_root.to_path_buf(),
        )
        .with_authority_generation(authority),
    );
    let command_execution = std::sync::Arc::new(RuntimeManagedCommandExecutionRouteV1::new(
        Arc::clone(&planner),
        Arc::clone(&broker),
        execution_temp_root.to_path_buf(),
    ));
    let bundle = sigil_resource_authority::factory::ResourceAuthorityServiceFactoryV1::new_with_borrowed_native_save(
        authority,
        storage.clone() as Arc<dyn ManagedStorageServiceV1>,
        file_access.clone() as Arc<dyn ManagedFileAccessServiceV1>,
        borrowed_native_save,
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
        )
        .with_optional_product_updater(product_updater);
    let artifact_staging_grant = crate::managed_storage_writer::grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStaging,
        0x76,
        authority,
        cutover_manifest_hash,
    );
    let artifact_store_grant = crate::managed_storage_writer::grant_for_channel_with_context(
        StorageWriterChannelV1::ArtifactStore,
        0x76,
        authority,
        cutover_manifest_hash,
    );
    let artifact_retire_authority = std::sync::Arc::new(
        sigil_resource_authority::maintenance::ArtifactRetireAuthorityV1::new(
            authority,
            artifact_staging_grant.grant_hash,
            artifact_store_grant.grant_hash,
        ),
    );
    let storage_writer = std::sync::Arc::new(
        ManagedStorageWriterAdapterV1::with_storage_issuer(
            storage,
            state_anchor.to_path_buf(),
            cutover_manifest_hash,
            std::sync::Arc::clone(&broker),
        )
        .with_artifact_retire_authority(artifact_retire_authority),
    );
    let plugin_hook_execution = std::sync::Arc::new(
        RuntimeManagedPluginHookExecutionRouteV1::new(
            Arc::clone(&extension_execution),
            execution_config,
            Arc::clone(&storage_writer),
        )
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::ExecutionConfigurationInvalid(error.to_string())
        })?,
    );
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
        plugin_hook_execution,
        command_execution,
        authority_generation: authority,
        borrowed_workspace_registry: registry,
    })
}

fn hash_path_binding(label: &str, state_anchor: &Path) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update(state_anchor.as_os_str().to_string_lossy().as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
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

/// Builds and publishes the current-schema authority composition for one valid boot surface.
///
/// The readiness manifest is computed once against an isolated provisional composition, then the
/// production composition is rebound to the resulting content hash so storage grants and the
/// persisted cutover manifest carry the same source binding. Probes perform real admission and
/// settlement, so their provisional journal must not contaminate the production journal before
/// the source binding is frozen.
pub fn compose_current_boot_authority(
    config_path: &std::path::Path,
    state_anchor: &std::path::Path,
    cache_root: &std::path::Path,
    execution_temp_root: &std::path::Path,
) -> Result<
    (
        crate::r71_global_cutover::RuntimeGlobalCutoverV1,
        RuntimeAuthorityCompositionV1,
    ),
    BootAuthorityErrorV1,
> {
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
    let instance_id = format!(
        "sigil:{}",
        sigil_kernel::external::sha256_hex(config_path.to_string_lossy().as_bytes())
    );
    let authority = sigil_kernel::resource::AuthorityGeneration {
        epoch: 1,
        instance_hash: CanonicalHash::from_bytes([0x75; 32]),
    };
    let declared = [
        Ch::ApplicationControlLog,
        Ch::SessionLog,
        Ch::SessionLifecycleLog,
        Ch::InputHistory,
        Ch::DurableMemory,
        Ch::SessionCatalog,
        Ch::ArtifactStaging,
        Ch::ArtifactStore,
        Ch::AdapterDurableState,
        Ch::AdapterEgressDisclosure,
        Ch::AdapterIdempotencyLedger,
    ];
    let planner = std::sync::Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
        crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
    ));
    let provisional_root =
        tempfile::tempdir().map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    let provisional_state_anchor = provisional_root.path().join("state");
    std::fs::create_dir_all(provisional_state_anchor.join("cache"))
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    let provisional_hash = hash_path_binding("cutover-provisional-v1", config_path);
    let provisional = compose_runtime_authority_with_product_updater(
        &provisional_state_anchor,
        cache_root,
        execution_temp_root,
        config_path,
        provisional_hash,
        planner,
        &declared,
    )
    .map_err(BootAuthorityErrorV1::Composition)?;
    let recovery = crate::resource_recovery_surface::RuntimeResourceRecoveryFacadeV1::new();
    let first = crate::r71_global_cutover::RuntimeGlobalCutoverV1::evaluate(
        instance_id.clone(),
        1,
        authority,
        &provisional.services,
        &recovery,
        sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema,
    );
    first.gate().map_err(|error| {
        BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(
            error.clone(),
        ))
    })?;
    drop(provisional);
    let planner = std::sync::Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
        crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
    ));
    let composition = compose_runtime_authority_with_product_updater(
        state_anchor,
        cache_root,
        execution_temp_root,
        config_path,
        first.manifest().manifest_hash,
        planner,
        &declared,
    )
    .map_err(BootAuthorityErrorV1::Composition)?;
    let decision = crate::r71_global_cutover::RuntimeGlobalCutoverV1::evaluate(
        instance_id,
        1,
        authority,
        &composition.services,
        &recovery,
        sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema,
    );
    decision.gate().map_err(|error| {
        BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(
            error.clone(),
        ))
    })?;
    let manifest_path = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(".sigil-cutover-manifest.json");
    if manifest_path.exists() {
        let existing =
            crate::r71_global_cutover::RuntimeGlobalCutoverV1::load_and_validate_manifest(
                &manifest_path,
            )
            .map_err(|error| {
                BootAuthorityErrorV1::Cutover(
                    crate::r71_global_cutover::CutoverBootErrorV1::Persistence(error),
                )
            })?;
        if existing != *decision.manifest() {
            return Err(BootAuthorityErrorV1::Cutover(
                crate::r71_global_cutover::CutoverBootErrorV1::Guard(
                    sigil_kernel::cutover_manifest::CutoverErrorV1::AlreadyPublished,
                ),
            ));
        }
    } else {
        decision.save_manifest(&manifest_path).map_err(|error| {
            BootAuthorityErrorV1::Cutover(
                crate::r71_global_cutover::CutoverBootErrorV1::Persistence(error),
            )
        })?;
    }
    Ok((decision, composition))
}

/// One-call boot attach shared by CLI headless/machine and HTTP serve: publishes the current
/// epoch for a valid config, prepares the authority anchors and composes the authority surface
/// once, then attaches both to the run services (fail closed on any step). Missing or malformed
/// config keeps the legacy epoch-only recovery path.
pub fn attach_boot_authority_to_services(
    services: crate::application_run::ApplicationRunServices,
    config_path: &std::path::Path,
    workspace_root: &std::path::Path,
) -> Result<crate::application_run::ApplicationRunServices, BootAuthorityErrorV1> {
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
    let (cutover, composition) = compose_current_boot_authority(
        config_path,
        &paths.state_root,
        &paths.cache_root,
        &paths.scratch_root,
    )?;
    composition
        .activate_workspace(workspace_root)
        .map_err(BootAuthorityErrorV1::Config)?;
    let services = services.with_global_cutover(cutover);
    services.require_cutover_or_fail().map_err(|error| {
        BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(error))
    })?;
    services
        .admit_session_open(sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema)
        .map_err(|error| {
            BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(
                error,
            ))
        })?;
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

    #[test]
    fn r71_current_boot_publishes_one_green_current_manifest_and_replays_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
        let state = dir.path().join("state");
        let cache = state.join("cache");
        let exec = dir.path().join("exec");
        std::fs::create_dir_all(&cache).expect("cache");
        std::fs::create_dir_all(&exec).expect("exec");
        let (first, _composition) =
            compose_current_boot_authority(&config, &state, &cache, &exec).expect("current boot");
        assert!(first.is_current_schema_ready());
        assert_eq!(first.manifest().mandatory_readiness.len(), 18);
        assert_eq!(
            first.manifest().selected_epoch,
            sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
        );
        let (second, _composition) =
            compose_current_boot_authority(&config, &state, &cache, &exec).expect("replay");
        assert_eq!(first.manifest(), second.manifest());
    }

    #[test]
    fn r71_reopen_ignores_settled_admission_after_source_bound_grant_rollover() {
        use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        let exec = dir.path().join("exec");
        std::fs::create_dir_all(state.join("cache")).expect("cache dir");
        std::fs::create_dir_all(&exec).expect("exec dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
                .expect("state mode");
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700))
                .expect("exec mode");
        }

        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let first = compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x55; 32]),
            planner,
            &[Ch::SessionLog],
        )
        .expect("first composition");
        let recovery = RuntimeResourceRecoveryFacadeV1::new();
        let first_probes = probe_mandatory_adapters(
            &first.services,
            &recovery,
            CanonicalHash::from_bytes([0x55; 32]),
            1,
        );
        assert!(
            first_probes
                .iter()
                .find(|probe| probe.adapter == MandatoryAdapterKindV1::StorageSessionLog)
                .is_some_and(|probe| probe.passed)
        );
        drop(first);

        // A new cutover manifest rolls the source-bound grants. The old startup admission is
        // already terminal and must remain historical evidence, not make the journal look
        // corrupt during the next authority composition.
        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let second = compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x56; 32]),
            planner,
            &[Ch::SessionLog],
        )
        .expect("settled historical admission must not block grant rollover");
        let second_probes = probe_mandatory_adapters(
            &second.services,
            &recovery,
            CanonicalHash::from_bytes([0x56; 32]),
            1,
        );
        assert!(
            second_probes
                .iter()
                .find(|probe| probe.adapter == MandatoryAdapterKindV1::StorageSessionLog)
                .is_some_and(|probe| probe.passed)
        );
    }

    #[test]
    fn r71_reopen_recovers_pending_v2_admission_after_source_bound_grant_rollover() {
        use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;

        let dir = tempfile::tempdir().expect("tempdir");
        let state = dir.path().join("state");
        let exec = dir.path().join("exec");
        std::fs::create_dir_all(state.join("cache")).expect("cache dir");
        std::fs::create_dir_all(&exec).expect("exec dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700))
                .expect("state mode");
            std::fs::set_permissions(&exec, std::fs::Permissions::from_mode(0o700))
                .expect("exec mode");
        }

        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let first = compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x55; 32]),
            planner,
            &[Ch::SessionLog],
        )
        .expect("first composition");
        let pending = first
            .storage_writer
            .acquire_named(Ch::SessionLog, "pending-session")
            .expect("pending admission");
        first
            .storage_writer
            .write_record(&pending, b"{\"seq\":1}")
            .expect("pending physical write");
        let pending_path = pending.path().to_path_buf();
        let records_before =
            std::fs::read(pending_path.join("records.jsonl")).expect("pending physical records");
        drop(pending);
        drop(first);

        let planner: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionPlannerV1> =
            Arc::new(crate::r71_shadow_planner::ShadowPlannerV1::new(
                crate::r71_shadow_planner::ShadowPlannerConfigV1::default(),
            ));
        let second = compose_runtime_authority(
            &state,
            &exec,
            CanonicalHash::from_bytes([0x56; 32]),
            planner,
            &[Ch::SessionLog],
        )
        .expect("pending historical admission must be reconciled across grant rollover");
        assert_eq!(
            std::fs::read(pending_path.join("records.jsonl")).expect("records retained"),
            records_before
        );
        let new_lease = second
            .storage_writer
            .acquire_named(Ch::SessionLog, "after-recovery")
            .expect("new admissions unblocked");
        second
            .storage_writer
            .finalize(new_lease)
            .expect("new admission finalizes");
    }
}
