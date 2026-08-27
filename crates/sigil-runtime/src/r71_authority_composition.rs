//! RFC-0071 section 18 R71.6: production authority composition spine.
//!
//! The only place a boot surface turns verified bootstrap anchors + declared writer channels
//! into the composed runtime surface (services, storage writer adapter, authority-backed file
//! access). Declaring a writer channel registers exactly its grant: the cutover probe then
//! reflects what is composed and nothing more. Real authority services only - no stub in the
//! production path (the capability issuer, planner and projection facade are host-injected
//! because their production construction belongs to kernel/boot owners).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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
    file_access_impl:
        std::sync::Arc<sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1>,
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
        let capsule = self
            .borrowed_workspace_registry
            .lock()
            .map_err(|_| "borrowed workspace registry is poisoned".to_owned())?
            .activate_workspace(
                "sigil",
                workspace_id.as_str().to_owned(),
                workspace_root,
                self.authority_generation,
            )
            .map_err(|error| error.to_string())?;
        self.file_access_impl
            .reconcile_file_delete_journal()
            .map_err(|error| error.to_string())?;
        Ok(capsule)
    }
}

/// The indivisible current-schema boot result. A product surface may consume this value, but it
/// must not independently reload configuration, resolve authority paths, or activate a second
/// workspace registration.
pub struct RuntimeCurrentBootTransactionV1 {
    config: sigil_kernel::RootConfig,
    workspace_root: PathBuf,
    resolved_paths: crate::paths::SigilPaths,
    cutover: crate::r71_global_cutover::RuntimeGlobalCutoverV1,
    composition: RuntimeAuthorityCompositionV1,
    workspace_registration:
        sigil_resource_authority::borrowed::BorrowedWorkspaceRegistrationCapsuleV1,
}

impl RuntimeCurrentBootTransactionV1 {
    /// Returns the exact validated configuration consumed by this boot.
    #[must_use]
    pub fn config(&self) -> &sigil_kernel::RootConfig {
        &self.config
    }

    /// Returns the frozen effective workspace root.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Returns the frozen authority path view.
    #[must_use]
    pub fn resolved_paths(&self) -> &crate::paths::SigilPaths {
        &self.resolved_paths
    }

    /// Returns the immutable published cutover decision.
    #[must_use]
    pub fn cutover(&self) -> &crate::r71_global_cutover::RuntimeGlobalCutoverV1 {
        &self.cutover
    }

    /// Returns the already activated authority composition.
    #[must_use]
    pub fn composition(&self) -> &RuntimeAuthorityCompositionV1 {
        &self.composition
    }

    /// Returns the authority-issued workspace registration capsule.
    #[must_use]
    pub fn workspace_registration(
        &self,
    ) -> &sigil_resource_authority::borrowed::BorrowedWorkspaceRegistrationCapsuleV1 {
        &self.workspace_registration
    }

    /// Transfers the transaction's owned values to a surface that must retain them for the
    /// lifetime of its worker and renderer state.
    #[must_use]
    pub fn into_published_parts(
        self,
    ) -> (
        sigil_kernel::RootConfig,
        PathBuf,
        crate::paths::SigilPaths,
        crate::r71_global_cutover::RuntimeGlobalCutoverV1,
        RuntimeAuthorityCompositionV1,
        sigil_resource_authority::borrowed::BorrowedWorkspaceRegistrationCapsuleV1,
    ) {
        (
            self.config,
            self.workspace_root,
            self.resolved_paths,
            self.cutover,
            self.composition,
            self.workspace_registration,
        )
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

/// Validated, immutable configuration input for one authority composition.
///
/// The only public acquisition path is [`Self::load`]. The parsed config, captured launch cwd,
/// effective workspace identity, source-file identity, and every authority storage root are
/// frozen together; callers cannot inject a second workspace root after validation.
#[derive(Clone)]
pub struct ValidatedAuthorityConfigSnapshotV1 {
    config_path: PathBuf,
    config: sigil_kernel::RootConfig,
    workspace_root: PathBuf,
    launch_cwd: PathBuf,
    resolved_paths: crate::paths::SigilPaths,
    config_path_identity: CanonicalHash,
    workspace_identity: CanonicalHash,
    config_hash: CanonicalHash,
}

/// Reads the configuration bytes and observes its identity through the same no-follow handle.
/// The returned payload is the only input used for parsing, so an atomic replacement between
/// metadata inspection and parsing cannot create a split-brain boot.
fn load_config_payload_with_identity(
    config_path: &Path,
) -> Result<Option<(Vec<u8>, CanonicalHash)>, BootAuthorityErrorV1> {
    use std::io::Read;

    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(config_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BootAuthorityErrorV1::Config(error.to_string())),
        }
    };

    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt;
        match std::fs::OpenOptions::new()
            .read(true)
            .share_mode(
                windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE
                    | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_DELETE,
            )
            .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(config_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(BootAuthorityErrorV1::Config(error.to_string())),
        }
    };

    #[cfg(not(any(unix, windows)))]
    let file = match std::fs::File::open(config_path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(BootAuthorityErrorV1::Config(error.to_string())),
    };

    let metadata = file
        .metadata()
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    if !metadata.is_file() {
        return Err(BootAuthorityErrorV1::Config(
            "authority config is not a regular file".to_owned(),
        ));
    }
    #[cfg(windows)]
    let identity =
        sigil_resource_authority::identity::canonical_identity_from_handle(config_path, &file)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?
            .digest;
    #[cfg(not(windows))]
    let identity = sigil_resource_authority::identity::canonical_identity_from_metadata(
        config_path,
        &metadata,
    )
    .digest;
    let mut raw = Vec::new();
    file.take(2 * 1024 * 1024)
        .read_to_end(&mut raw)
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    Ok(Some((raw, identity)))
}

impl ValidatedAuthorityConfigSnapshotV1 {
    fn from_loaded_with_identity(
        config_path: &Path,
        config: sigil_kernel::RootConfig,
        launch_cwd: &Path,
        config_path_identity: CanonicalHash,
    ) -> Result<Self, BootAuthorityErrorV1> {
        let config_path = std::fs::canonicalize(config_path)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        let launch_cwd = std::fs::canonicalize(launch_cwd)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        let workspace_root =
            sigil_kernel::resolve_workspace_root(&config_path, &launch_cwd, &config.workspace.root);
        let workspace_root = std::fs::canonicalize(workspace_root)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        let resolved_paths =
            crate::resolve_sigil_paths(&config.storage, &config.session, &workspace_root);
        let workspace_identity =
            sigil_resource_authority::identity::canonical_identity(&workspace_root)
                .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?
                .digest;
        let encoded = serde_json::to_vec(&config)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        let config_hash = snapshot_binding_hash(
            &encoded,
            &config_path,
            &launch_cwd,
            &workspace_root,
            config_path_identity,
            workspace_identity,
            &resolved_paths,
        );
        Ok(Self {
            config_path,
            config,
            workspace_root,
            launch_cwd,
            resolved_paths,
            config_path_identity,
            workspace_identity,
            config_hash,
        })
    }

    /// Loads one valid snapshot. Missing, non-regular, or malformed config is unavailable to
    /// current-schema boot; the caller must route the typed error to setup/recovery UI or abort
    /// the headless request. There is no epoch-only fallback.
    pub fn load(
        config_path: &Path,
        launch_cwd: &Path,
    ) -> Result<Option<Self>, BootAuthorityErrorV1> {
        let Some((raw, config_path_identity)) = load_config_payload_with_identity(config_path)?
        else {
            return Ok(None);
        };
        let raw = std::str::from_utf8(&raw)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        let config = sigil_kernel::RootConfig::parse_with_model_request_env(raw)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        Self::from_loaded_with_identity(config_path, config, launch_cwd, config_path_identity)
            .map(Some)
    }

    #[must_use]
    pub(crate) fn config(&self) -> &sigil_kernel::RootConfig {
        &self.config
    }

    #[must_use]
    pub(crate) fn config_path(&self) -> &Path {
        &self.config_path
    }

    #[must_use]
    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    #[must_use]
    pub(crate) fn config_hash(&self) -> CanonicalHash {
        self.config_hash
    }

    #[must_use]
    pub(crate) fn resolved_paths(&self) -> &crate::paths::SigilPaths {
        &self.resolved_paths
    }
}

impl std::fmt::Debug for ValidatedAuthorityConfigSnapshotV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidatedAuthorityConfigSnapshotV1")
            .field("config_path", &self.config_path)
            .field("workspace_root", &self.workspace_root)
            .field("launch_cwd", &self.launch_cwd)
            .field("config_path_identity", &self.config_path_identity)
            .field("workspace_identity", &self.workspace_identity)
            .field("config_hash", &self.config_hash)
            .finish_non_exhaustive()
    }
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
pub(crate) fn compose_runtime_authority_with_product_updater(
    state_anchor: &Path,
    cache_root: &Path,
    execution_temp_root: &Path,
    config_snapshot: &ValidatedAuthorityConfigSnapshotV1,
    cutover_manifest_hash: CanonicalHash,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    let execution_config = config_snapshot.config().execution.clone();
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
            config_snapshot.config_path(),
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
        sigil_resource_authority::file_access::AuthorityManagedFileAccessServiceV1::new_with_journal(
            Arc::clone(&registry),
            state_anchor.join("file-delete-quarantine"),
            state_anchor.join("file-delete.journal.json"),
            bootstrap_manifest_hash,
            journal_instance_hash,
        )
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::JournalUnavailable(error.to_string())
        })?,
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
        file_access_impl,
    })
}

fn hash_path_binding(label: &str, state_anchor: &Path) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(label.as_bytes());
    hasher.update(state_anchor.as_os_str().to_string_lossy().as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

fn ensure_authority_anchors(paths: &crate::paths::SigilPaths) -> Result<(), BootAuthorityErrorV1> {
    for anchor in [&paths.state_root, &paths.scratch_root] {
        std::fs::create_dir_all(anchor)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        sigil_kernel::secure_private_path_permissions(anchor)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    }
    std::fs::create_dir_all(paths.state_root.join("cache"))
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    sigil_kernel::secure_private_path_permissions(&paths.state_root.join("cache"))
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    Ok(())
}

fn snapshot_binding_hash(
    encoded_config: &[u8],
    config_path: &Path,
    launch_cwd: &Path,
    workspace_root: &Path,
    config_path_identity: CanonicalHash,
    workspace_identity: CanonicalHash,
    paths: &crate::paths::SigilPaths,
) -> CanonicalHash {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"validated-authority-config-snapshot-v2");
    hasher.update(encoded_config);
    for path in [
        config_path,
        launch_cwd,
        workspace_root,
        &paths.state_root,
        &paths.cache_root,
        &paths.workspace_state_root,
        &paths.workspace_cache_root,
        &paths.scratch_root,
    ] {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update([0]);
    }
    hasher.update(config_path_identity.as_bytes());
    hasher.update(workspace_identity.as_bytes());
    CanonicalHash::from_bytes(hasher.finalize().into())
}

const AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AuthorityConfigGenerationRecordV1 {
    schema_version: u32,
    config_hash: CanonicalHash,
    generation: u64,
}

fn host_application_instance_id(config_path: &Path) -> String {
    format!(
        "sigil:{}",
        sigil_kernel::external::sha256_hex(config_path.to_string_lossy().as_bytes())
    )
}

fn load_authority_config_generation(
    bootstrap: &sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1,
    publication: &sigil_resource_authority::bootstrap::AuthorityBootstrapPublicationGuard,
) -> Result<Option<AuthorityConfigGenerationRecordV1>, BootAuthorityErrorV1> {
    let record = bootstrap
        .read_json::<AuthorityConfigGenerationRecordV1>(
            publication,
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    let Some(record) = record else {
        let cutover_exists = bootstrap
            .read_bytes(
                publication,
                sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
            )
            .map_err(BootAuthorityErrorV1::Bootstrap)?
            .is_some();
        if cutover_exists || !bootstrap.was_created_for_this_open() {
            return Err(BootAuthorityErrorV1::Bootstrap(
                sigil_resource_authority::bootstrap::BootstrapErrorV1::ReconciliationRequired(
                    "authority config generation metadata is missing".to_owned(),
                ),
            ));
        }
        return Ok(None);
    };
    if record.schema_version != AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION || record.generation == 0
    {
        return Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(
                "authority config generation record is invalid".to_owned(),
            ),
        ));
    }
    Ok(Some(record))
}

fn load_or_advance_authority_config_generation(
    bootstrap: &sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1,
    publication: &sigil_resource_authority::bootstrap::AuthorityBootstrapPublicationGuard,
    config_hash: CanonicalHash,
) -> Result<u64, BootAuthorityErrorV1> {
    let current = load_authority_config_generation(bootstrap, publication)?;
    let generation = match current.as_ref() {
        Some(record) if record.config_hash == config_hash => record.generation,
        Some(record) => record.generation.checked_add(1).ok_or_else(|| {
            BootAuthorityErrorV1::Config("authority config generation overflow".to_owned())
        })?,
        None => 1,
    };
    if current
        .as_ref()
        .is_none_or(|record| record.config_hash != config_hash || record.generation != generation)
    {
        let record = AuthorityConfigGenerationRecordV1 {
            schema_version: AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION,
            config_hash,
            generation,
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
        bootstrap
            .publish_bytes(
                publication,
                sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
                &bytes,
            )
            .map_err(BootAuthorityErrorV1::Bootstrap)?;
    }
    Ok(generation)
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
    #[error("authority bootstrap failed: {0}")]
    Bootstrap(sigil_resource_authority::bootstrap::BootstrapErrorV1),
}

fn build_current_boot_transaction(
    config_snapshot: ValidatedAuthorityConfigSnapshotV1,
) -> Result<RuntimeCurrentBootTransactionV1, BootAuthorityErrorV1> {
    let config = config_snapshot.config().clone();
    let workspace_root = config_snapshot.workspace_root().to_path_buf();
    let resolved_paths = config_snapshot.resolved_paths().clone();
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_canonical_config_path(
            config_snapshot.config_path(),
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    let publication = bootstrap
        .acquire_publication()
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    let (cutover, composition) = compose_current_boot_authority_locked(
        &config_snapshot,
        &resolved_paths.state_root,
        &resolved_paths.cache_root,
        &resolved_paths.scratch_root,
        &bootstrap,
        &publication,
    )?;
    let workspace_registration = composition
        .activate_workspace(&workspace_root)
        .map_err(BootAuthorityErrorV1::Config)?;
    cutover.gate().map_err(|error| {
        BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(
            error.clone(),
        ))
    })?;
    publish_current_boot_manifest(&bootstrap, &publication, &cutover)?;
    drop(publication);
    Ok(RuntimeCurrentBootTransactionV1 {
        config,
        workspace_root,
        resolved_paths,
        cutover,
        composition,
        workspace_registration,
    })
}

/// Loads and publishes one current-schema boot transaction. Missing or malformed configuration
/// is a typed failure and never selects a legacy epoch.
pub fn boot_current_schema(
    config_path: &Path,
    launch_cwd: &Path,
) -> Result<RuntimeCurrentBootTransactionV1, BootAuthorityErrorV1> {
    let snapshot =
        ValidatedAuthorityConfigSnapshotV1::load(config_path, launch_cwd)?.ok_or_else(|| {
            BootAuthorityErrorV1::Config(
                "current-schema authority config is unavailable".to_owned(),
            )
        })?;
    build_current_boot_transaction(snapshot)
}

/// Returns the host-private current cutover pointer path for one config identity. Product
/// surfaces use this only for diagnostics/tests; they never derive the path from the config
/// parent or publish metadata themselves.
pub fn authority_bootstrap_manifest_path(
    config_path: &Path,
) -> Result<PathBuf, BootAuthorityErrorV1> {
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
            config_path,
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    Ok(bootstrap
        .path(sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer))
}

/// Builds the current-schema authority composition for one valid boot surface. Publication of the
/// cutover manifest belongs to [`build_current_boot_transaction`], after workspace activation and
/// journal reconciliation have completed.
///
/// The readiness manifest is computed once against an isolated provisional composition, then the
/// production composition is rebound to the resulting content hash so storage grants and the
/// persisted cutover manifest carry the same source binding. Probes perform real admission and
/// settlement, so their provisional journal must not contaminate the production journal before
/// the source binding is frozen.
pub fn compose_current_boot_authority(
    config_snapshot: &ValidatedAuthorityConfigSnapshotV1,
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
    let bootstrap =
        sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_canonical_config_path(
            config_snapshot.config_path(),
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    let publication = bootstrap
        .acquire_publication()
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    compose_current_boot_authority_locked(
        config_snapshot,
        state_anchor,
        cache_root,
        execution_temp_root,
        &bootstrap,
        &publication,
    )
}

fn compose_current_boot_authority_locked(
    config_snapshot: &ValidatedAuthorityConfigSnapshotV1,
    state_anchor: &std::path::Path,
    cache_root: &std::path::Path,
    execution_temp_root: &std::path::Path,
    bootstrap: &sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1,
    publication: &sigil_resource_authority::bootstrap::AuthorityBootstrapPublicationGuard,
) -> Result<
    (
        crate::r71_global_cutover::RuntimeGlobalCutoverV1,
        RuntimeAuthorityCompositionV1,
    ),
    BootAuthorityErrorV1,
> {
    let paths = config_snapshot.resolved_paths();
    ensure_authority_anchors(paths)?;
    if state_anchor != paths.state_root
        || cache_root != paths.cache_root
        || execution_temp_root != paths.scratch_root
    {
        return Err(BootAuthorityErrorV1::Config(
            "authority anchors do not match the frozen configuration snapshot".to_owned(),
        ));
    }
    use crate::managed_storage_writer::StorageWriterChannelV1 as Ch;
    let instance_id = host_application_instance_id(config_snapshot.config_path());
    let application_generation = load_or_advance_authority_config_generation(
        bootstrap,
        publication,
        config_snapshot.config_hash(),
    )?;
    let authority = sigil_kernel::resource::AuthorityGeneration {
        epoch: bootstrap.authority_epoch(),
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
    let provisional_hash =
        hash_path_binding("cutover-provisional-v1", config_snapshot.config_path());
    let provisional = compose_runtime_authority_with_product_updater(
        &provisional_state_anchor,
        cache_root,
        execution_temp_root,
        config_snapshot,
        provisional_hash,
        planner,
        &declared,
    )
    .map_err(BootAuthorityErrorV1::Composition)?;
    let recovery = crate::resource_recovery_surface::RuntimeResourceRecoveryFacadeV1::new();
    let first = crate::r71_global_cutover::RuntimeGlobalCutoverV1::evaluate_current_schema(
        instance_id.clone(),
        application_generation,
        authority,
        &provisional.services,
        &recovery,
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
        config_snapshot,
        first.manifest().manifest_hash,
        planner,
        &declared,
    )
    .map_err(BootAuthorityErrorV1::Composition)?;
    let decision = crate::r71_global_cutover::RuntimeGlobalCutoverV1::evaluate_current_schema(
        instance_id,
        application_generation,
        authority,
        &composition.services,
        &recovery,
    );
    decision.gate().map_err(|error| {
        BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(
            error.clone(),
        ))
    })?;
    Ok((decision, composition))
}

fn publish_current_boot_manifest(
    bootstrap: &sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1,
    publication: &sigil_resource_authority::bootstrap::AuthorityBootstrapPublicationGuard,
    decision: &crate::r71_global_cutover::RuntimeGlobalCutoverV1,
) -> Result<(), BootAuthorityErrorV1> {
    let generation = bootstrap
        .read_json::<AuthorityConfigGenerationRecordV1>(
            publication,
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?
        .ok_or_else(|| {
            BootAuthorityErrorV1::Bootstrap(
                sigil_resource_authority::bootstrap::BootstrapErrorV1::ReconciliationRequired(
                    "authority config generation is missing before cutover publication".to_owned(),
                ),
            )
        })?;
    if generation.generation != decision.manifest().application_generation {
        return Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::ReconciliationRequired(format!(
                "authority config generation {} does not match cutover generation {}",
                generation.generation,
                decision.manifest().application_generation
            )),
        ));
    }
    let existing = bootstrap
        .read_bytes(
            publication,
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?
        .map(|bytes| {
            crate::r71_global_cutover::RuntimeGlobalCutoverV1::validate_manifest_bytes(&bytes)
                .map_err(|error| {
                    BootAuthorityErrorV1::Bootstrap(
                        sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(
                            format!("cutover pointer validation failed: {error}"),
                        ),
                    )
                })
        })
        .transpose()?;
    if let Some(existing) = existing {
        if existing == *decision.manifest() {
            return Ok(());
        }
        let is_current_generation_advance = existing.selected_epoch
            == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
            && decision.manifest().selected_epoch
                == sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
            && existing.application_instance_id == decision.manifest().application_instance_id
            && decision.manifest().application_generation > existing.application_generation;
        if !is_current_generation_advance {
            // The application identity is host-owned and stable. A changed manifest for the
            // same generation, a generation rollback, a different owner, or a legacy decision is
            // fixed-forward and cannot replace the current pointer.
            return Err(BootAuthorityErrorV1::Cutover(
                crate::r71_global_cutover::CutoverBootErrorV1::Guard(
                    sigil_kernel::cutover_manifest::CutoverErrorV1::AlreadyPublished,
                ),
            ));
        }
    }
    // The file is a current-instance pointer. The decision itself remains content-addressed and
    // immutable; a validated reboot atomically publishes the new instance's decision.
    let bytes = serde_json::to_vec(decision.manifest())
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    bootstrap
        .publish_bytes(
            publication,
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
            &bytes,
        )
        .map_err(BootAuthorityErrorV1::Bootstrap)?;
    Ok(())
}

/// One-call boot attach shared by CLI headless/machine and HTTP serve: publishes the current
/// epoch for a valid config, prepares the authority anchors and composes the authority surface
/// once, then attaches both to the run services (fail closed on any step). Missing or malformed
/// config is a typed unavailable failure; there is no epoch-only recovery path.
pub fn attach_boot_authority_to_services(
    services: crate::application_run::ApplicationRunServices,
    config_path: &std::path::Path,
    launch_cwd: &std::path::Path,
) -> Result<crate::application_run::ApplicationRunServices, BootAuthorityErrorV1> {
    let transaction = boot_current_schema(config_path, launch_cwd)?;
    let (_, _, _, cutover, composition, _) = transaction.into_published_parts();
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
        RuntimeExecutionSeamV1, RuntimeFileAccessSeamV1, RuntimeGlobalCutoverV1,
        probe_mandatory_adapters,
    };
    use crate::resource_recovery_surface::RuntimeResourceRecoveryFacadeV1;
    use sigil_kernel::cutover_manifest::MandatoryAdapterKindV1;

    fn write_r71_boot_config(config: &Path) {
        let fixture_root = config.parent().expect("config parent");
        let state = fixture_root.join(".r71-test-state");
        let cache = fixture_root.join(".r71-test-cache");
        std::fs::create_dir_all(&cache).expect("test cache");
        std::fs::write(
            config,
            format!(
                "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
                state.display(),
                cache.display(),
            ),
        )
        .expect("config");
    }

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
        // Composition boot resolves shared state/cache defaults in a few nested adapters. Keep
        // this replay assertion behind the runtime test environment lock so parallel fixtures
        // that temporarily override those process-wide roots cannot invalidate the second boot.
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
        let state = dir.path().join("state");
        let cache = state.join("cache");
        std::fs::create_dir_all(&cache).expect("cache");
        let _state_env = crate::test_env::EnvScope::set(crate::SIGIL_STATE_HOME_ENV, &state);
        let _cache_env = crate::test_env::EnvScope::set(crate::SIGIL_CACHE_HOME_ENV, &cache);
        let config_snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
            .expect("load snapshot")
            .expect("snapshot");
        let exec = config_snapshot.resolved_paths().scratch_root.clone();
        std::fs::create_dir_all(&exec).expect("exec");
        let (first, _composition) =
            compose_current_boot_authority(&config_snapshot, &state, &cache, &exec)
                .expect("current boot");
        assert!(first.is_current_schema_ready());
        assert_eq!(first.manifest().mandatory_readiness.len(), 18);
        assert_eq!(
            first.manifest().selected_epoch,
            sigil_kernel::cutover_manifest::StartupEpochV1::NewCurrentSchema
        );
        let (second, _composition) =
            compose_current_boot_authority(&config_snapshot, &state, &cache, &exec)
                .expect("replay");
        assert_eq!(first.manifest(), second.manifest());
    }

    #[test]
    fn r71_current_boot_advances_manifest_for_a_new_persisted_config_generation() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        let state_a = dir.path().join("state-a");
        let cache_a = state_a.join("cache");
        let state_b = dir.path().join("state-b");
        let cache_b = state_b.join("cache");
        std::fs::create_dir_all(&cache_a).expect("cache a");
        std::fs::create_dir_all(&cache_b).expect("cache b");
        let config_a = format!(
            "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"old-model\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
            state_a.display(),
            cache_a.display(),
        );
        std::fs::write(&config, config_a).expect("config");

        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        let first_instance = first.cutover().manifest().application_instance_id.clone();
        let first_generation = first.cutover().manifest().application_generation;
        let updated = std::fs::read_to_string(&config)
            .expect("read config")
            .replace("old-model", "new-model")
            .replace(
                &state_a.display().to_string(),
                &state_b.display().to_string(),
            )
            .replace(
                &cache_a.display().to_string(),
                &cache_b.display().to_string(),
            );
        std::fs::write(&config, updated).expect("update config");
        let second = boot_current_schema(&config, dir.path()).expect("updated config boot");

        assert_eq!(
            first_instance,
            second.cutover().manifest().application_instance_id
        );
        assert_eq!(second.config().agent.model, "new-model");
        assert_eq!(second.resolved_paths().state_root, state_b);
        assert_eq!(
            second.cutover().manifest().application_generation,
            first_generation + 1
        );
        assert_eq!(
            sigil_kernel::RootConfig::load(&config)
                .expect("reload persisted config")
                .agent
                .model,
            "new-model"
        );
        let published = RuntimeGlobalCutoverV1::load_and_validate_manifest(
            &authority_bootstrap_manifest_path(&config).expect("authority bootstrap manifest path"),
        )
        .expect("published manifest");
        assert_eq!(published, *second.cutover().manifest());
    }

    #[test]
    fn r71_bootstrap_metadata_missing_requires_typed_reconciliation() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        write_r71_boot_config(&config);
        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        drop(first);
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        std::fs::remove_file(bootstrap.path(
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
        ))
        .expect("remove generation metadata");

        let result = boot_current_schema(&config, dir.path());
        assert!(matches!(
            result,
            Err(BootAuthorityErrorV1::Bootstrap(
                sigil_resource_authority::bootstrap::BootstrapErrorV1::ReconciliationRequired(_)
            ))
        ));
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_bootstrap_metadata_corruption_is_typed_and_fail_closed() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        write_r71_boot_config(&config);
        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        drop(first);
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        std::fs::write(
            bootstrap.path(
                sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::AuthorityConfigGeneration,
            ),
            b"{corrupt",
        )
        .expect("corrupt generation metadata");

        let result = boot_current_schema(&config, dir.path());
        assert!(matches!(
            result,
            Err(BootAuthorityErrorV1::Bootstrap(
                sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
            ))
        ));
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_bootstrap_pointer_corruption_is_typed_and_fail_closed() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        write_r71_boot_config(&config);
        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        drop(first);
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        std::fs::write(
            bootstrap.path(
                sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
            ),
            b"{corrupt",
        )
        .expect("corrupt pointer metadata");

        let result = boot_current_schema(&config, dir.path());
        assert!(matches!(
            result,
            Err(BootAuthorityErrorV1::Bootstrap(
                sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
            ))
        ));
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[cfg(unix)]
    #[test]
    fn r71_bootstrap_pointer_symlink_is_rejected_before_publish() {
        use std::os::unix::fs::symlink;

        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        write_r71_boot_config(&config);
        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        drop(first);
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        let pointer = bootstrap.path(
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
        );
        std::fs::remove_file(&pointer).expect("remove pointer");
        let target = dir.path().join("pointer-target");
        std::fs::write(&target, b"not authority metadata").expect("target");
        symlink(&target, &pointer).expect("pointer symlink");

        let result = boot_current_schema(&config, dir.path());
        assert!(matches!(
            result,
            Err(BootAuthorityErrorV1::Bootstrap(
                sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(_)
            ))
        ));
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_bootstrap_pointer_loss_after_generation_publication_reconciles_on_reboot() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        write_r71_boot_config(&config);
        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        let expected = first.cutover().manifest().clone();
        drop(first);
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        std::fs::remove_file(bootstrap.path(
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
        ))
        .expect("simulate crash before pointer publication");

        let replay = boot_current_schema(&config, dir.path()).expect("reconcile reboot");
        assert_eq!(*replay.cutover().manifest(), expected);
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_bootstrap_pointer_is_outside_an_explicit_config_parent() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("workspace").join("sigil.toml");
        std::fs::create_dir_all(config.parent().expect("config parent")).expect("parent");
        std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
        let pointer = authority_bootstrap_manifest_path(&config).expect("pointer path");
        assert!(!pointer.starts_with(config.parent().expect("config parent")));
        assert!(pointer.ends_with(".sigil-cutover-manifest.json"));
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_bootstrap_ignores_config_parent_metadata_replacement() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config_parent = dir.path().join("workspace");
        std::fs::create_dir_all(&config_parent).expect("config parent");
        let config = config_parent.join("sigil.toml");
        std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
        let first = boot_current_schema(&config, dir.path()).expect("first boot");
        let expected = first.cutover().manifest().clone();
        drop(first);

        std::fs::write(
            config_parent.join("authority-config-generation.json"),
            b"{corrupt",
        )
        .expect("replace config-parent generation");
        std::fs::write(
            config_parent.join(".sigil-cutover-manifest.json"),
            b"{corrupt",
        )
        .expect("replace config-parent pointer");

        let replay = boot_current_schema(&config, dir.path()).expect("replay boot");
        assert_eq!(*replay.cutover().manifest(), expected);
        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_concurrent_boots_publish_monotonic_generation_under_one_lock() {
        use std::sync::{Arc, Barrier};

        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let config = dir.path().join("sigil.toml");
        let state_a = dir.path().join("state-a");
        let cache_a = state_a.join("cache");
        let state_b = dir.path().join("state-b");
        let cache_b = state_b.join("cache");
        std::fs::create_dir_all(&cache_a).expect("cache a");
        std::fs::create_dir_all(&cache_b).expect("cache b");
        let config_text = |state: &Path, cache: &Path, model: &str| {
            format!(
                "config_version = 2\n[workspace]\nroot = \".\"\n[storage]\nstate_root = \"{}\"\ncache_root = \"{}\"\n[agent]\nconnection = \"local-test\"\nmodel = \"{}\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = {{ source = \"none\" }}\n",
                state.display(),
                cache.display(),
                model,
            )
        };
        std::fs::write(&config, config_text(&state_a, &cache_a, "model-a")).expect("config a");
        let snapshot_a = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
            .expect("load snapshot a")
            .expect("snapshot a");
        std::fs::write(&config, config_text(&state_b, &cache_b, "model-b")).expect("config b");
        let snapshot_b = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
            .expect("load snapshot b")
            .expect("snapshot b");

        let barrier = Arc::new(Barrier::new(3));
        let barrier_a = Arc::clone(&barrier);
        let handle_a = std::thread::spawn(move || {
            barrier_a.wait();
            build_current_boot_transaction(snapshot_a)
        });
        let barrier_b = Arc::clone(&barrier);
        let handle_b = std::thread::spawn(move || {
            barrier_b.wait();
            build_current_boot_transaction(snapshot_b)
        });
        barrier.wait();
        assert!(handle_a.join().expect("boot a thread").is_ok());
        assert!(handle_b.join().expect("boot b thread").is_ok());

        let bootstrap =
            sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1::for_config_path(
                &config,
            )
            .expect("bootstrap store");
        let pointer = RuntimeGlobalCutoverV1::load_and_validate_manifest(&bootstrap.path(
            sigil_resource_authority::bootstrap::AuthorityBootstrapObjectClassV1::CutoverPointer,
        ))
        .expect("published pointer");
        assert_eq!(pointer.application_generation, 2);
        std::fs::remove_dir_all(bootstrap.root()).expect("cleanup bootstrap fixture");
    }

    #[test]
    fn r71_authority_snapshot_survives_config_replacement_without_split_brain() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_a = dir.path().join("workspace-a");
        let workspace_b = dir.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_a).expect("workspace a");
        std::fs::create_dir_all(&workspace_b).expect("workspace b");
        let config = dir.path().join("sigil.toml");
        let config_a = "config_version = 2\n[workspace]\nroot = \"workspace-a\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n";
        let config_b = config_a.replace("workspace-a", "workspace-b");
        let state = dir.path().join("state");
        let cache = state.join("cache");
        std::fs::create_dir_all(&cache).expect("cache");
        let _state_env = crate::test_env::EnvScope::set(crate::SIGIL_STATE_HOME_ENV, &state);
        let _cache_env = crate::test_env::EnvScope::set(crate::SIGIL_CACHE_HOME_ENV, &cache);
        std::fs::write(&config, config_a).expect("config a");
        let snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
            .expect("load snapshot")
            .expect("valid snapshot");
        let original_hash = snapshot.config_hash();
        std::fs::write(&config, config_b).expect("config b");

        assert_eq!(
            snapshot.workspace_root(),
            workspace_a
                .canonicalize()
                .expect("canonical workspace a")
                .as_path()
        );
        assert_eq!(snapshot.config_hash(), original_hash);
        let live_snapshot = ValidatedAuthorityConfigSnapshotV1::load(&config, dir.path())
            .expect("load live snapshot")
            .expect("live snapshot");
        assert_ne!(snapshot.config_hash(), live_snapshot.config_hash());
        let exec = snapshot.resolved_paths().scratch_root.clone();
        std::fs::create_dir_all(&exec).expect("exec");

        let (_cutover, composition) =
            compose_current_boot_authority(&snapshot, &state, &cache, &exec)
                .expect("compose from frozen snapshot");
        let capsule = composition
            .activate_workspace(snapshot.workspace_root())
            .expect("activate frozen workspace");
        let workspace_identity = sigil_resource_authority::identity::canonical_identity(
            &workspace_a.canonicalize().expect("canonical workspace"),
        )
        .expect("workspace identity")
        .digest;
        assert_eq!(capsule.root_identity_hash, workspace_identity);
        assert_ne!(capsule.root_identity_hash, snapshot.config_hash());
    }

    #[test]
    fn r71_snapshot_binding_changes_with_launch_cwd_for_default_workspace() {
        let _environment_guard = crate::test_env::lock();
        let dir = tempfile::tempdir().expect("tempdir");
        let workspace_a = dir.path().join("workspace-a");
        let workspace_b = dir.path().join("workspace-b");
        std::fs::create_dir_all(&workspace_a).expect("workspace a");
        std::fs::create_dir_all(&workspace_b).expect("workspace b");
        let config = dir.path().join("sigil.toml");
        std::fs::write(
            &config,
            "config_version = 2\n[workspace]\nroot = \".\"\n[agent]\nconnection = \"local-test\"\nmodel = \"test\"\n[connections.local-test]\nlabel = \"local\"\nprovider = \"custom\"\nprotocol = \"chat_completions\"\nbase_url = \"http://127.0.0.1:1\"\ncredential = { source = \"none\" }\n",
        )
        .expect("config");
        let snapshot_a = ValidatedAuthorityConfigSnapshotV1::load(&config, &workspace_a)
            .expect("load a")
            .expect("snapshot a");
        let snapshot_b = ValidatedAuthorityConfigSnapshotV1::load(&config, &workspace_b)
            .expect("load b")
            .expect("snapshot b");
        assert_ne!(snapshot_a.workspace_root(), snapshot_b.workspace_root());
        assert_ne!(snapshot_a.config_hash(), snapshot_b.config_hash());
        assert_ne!(snapshot_a.resolved_paths(), snapshot_b.resolved_paths());
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
