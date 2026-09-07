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
use sigil_kernel::resource::{AuthorityGeneration, CanonicalHash, ResourceAuthorityScopeV1};

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
    ) -> Result<
        sigil_resource_authority::borrowed::BorrowedWorkspaceRegistrationCapsuleV1,
        RuntimeAuthorityCompositionErrorV1,
    > {
        let workspace_id = sigil_kernel::stable_workspace_id(workspace_root).map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::ExecutionConfigurationInvalid(error.to_string())
        })?;
        let capsule = self
            .borrowed_workspace_registry
            .lock()
            .map_err(|_| {
                RuntimeAuthorityCompositionErrorV1::ExecutionConfigurationInvalid(
                    "borrowed workspace registry is poisoned".to_owned(),
                )
            })?
            .activate_workspace(
                "sigil",
                workspace_id.as_str().to_owned(),
                workspace_root,
                self.authority_generation,
            )
            .map_err(|error| {
                RuntimeAuthorityCompositionErrorV1::ExecutionConfigurationInvalid(error.to_string())
            })?;
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
    source_hash: CanonicalHash,
    config_hash: CanonicalHash,
}

/// Reads the configuration bytes and observes its identity through the same no-follow handle.
/// The returned payload is the only input used for parsing, so an atomic replacement between
/// metadata inspection and parsing cannot create a split-brain boot.
fn load_config_payload_with_identity(
    config_path: &Path,
) -> Result<Option<(Vec<u8>, CanonicalHash)>, BootAuthorityErrorV1> {
    use std::io::Read;
    // Normalize only the parent directories for the identity label. Following the final
    // component here would defeat the no-follow open below, while leaving `/var`-style parent
    // aliases unresolved makes the same file look different on macOS between boot phases.
    let identity_path = config_path
        .file_name()
        .and_then(|file_name| {
            config_path
                .parent()
                .and_then(|parent| parent.canonicalize().ok())
                .map(|parent| parent.join(file_name))
        })
        .unwrap_or_else(|| config_path.to_path_buf());

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
        sigil_resource_authority::identity::canonical_identity_from_handle(&identity_path, &file)
            .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?
            .digest;
    #[cfg(not(windows))]
    let identity = sigil_resource_authority::identity::canonical_identity_from_metadata(
        &identity_path,
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
        raw_config: &[u8],
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
        let config_hash = snapshot_binding_hash(
            raw_config,
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
            source_hash: sigil_process_observer::canonical_digest(raw_config),
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
        Self::from_loaded_with_identity(
            config_path,
            config,
            raw.as_bytes(),
            launch_cwd,
            config_path_identity,
        )
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
    fn source_hash(&self) -> CanonicalHash {
        self.source_hash
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
    authority: AuthorityGeneration,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    compose_runtime_authority_inner(
        state_anchor,
        execution_temp_root,
        cutover_manifest_hash,
        authority,
        planner,
        declared,
        None,
        sigil_kernel::ExecutionConfig::default(),
        1,
        None,
    )
}

/// Unit-test composition for physical managed execution. Shipping callers must obtain their
/// durable inventory from [`boot_current_schema`]; tests opt into the feature-gated in-memory
/// adapter explicitly so the production helper remains fail-closed.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn compose_runtime_authority_for_test_execution(
    state_anchor: &Path,
    execution_temp_root: &Path,
    cutover_manifest_hash: CanonicalHash,
    authority: AuthorityGeneration,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    compose_runtime_authority_inner(
        state_anchor,
        execution_temp_root,
        cutover_manifest_hash,
        authority,
        planner,
        declared,
        None,
        sigil_kernel::ExecutionConfig::default(),
        1,
        Some(Arc::new(
            sigil_resource_authority::InMemoryAuthorityProcessInventoryV1::default(),
        )),
    )
}

/// Composes the production authority surface with the independent product updater owner.
///
/// The updater cache is a trusted product-plane object, not a managed agent resource. Keeping
/// its owner explicit here lets the cutover probe verify the real writer attachment without
/// granting the updater an agent/session capability.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(crate) fn compose_runtime_authority_with_product_updater(
    state_anchor: &Path,
    cache_root: &Path,
    execution_temp_root: &Path,
    config_snapshot: &ValidatedAuthorityConfigSnapshotV1,
    cutover_manifest_hash: CanonicalHash,
    authority: AuthorityGeneration,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
    process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>,
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    compose_runtime_authority_with_product_updater_at_generation(
        state_anchor,
        cache_root,
        execution_temp_root,
        config_snapshot,
        cutover_manifest_hash,
        1,
        authority,
        planner,
        declared,
        process_inventory,
    )
}

/// Same composition entry with the exact application generation selected by the boot transaction.
/// The generation is part of every ApplicationCutoverRoot source binding and therefore cannot be
/// inferred from the authority epoch (the two counters have different owners).
#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_runtime_authority_with_product_updater_at_generation(
    state_anchor: &Path,
    cache_root: &Path,
    execution_temp_root: &Path,
    config_snapshot: &ValidatedAuthorityConfigSnapshotV1,
    cutover_manifest_hash: CanonicalHash,
    application_generation: u64,
    authority: AuthorityGeneration,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
    process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>,
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    let execution_config = config_snapshot.config().execution.clone();
    let mut composition = compose_runtime_authority_inner(
        state_anchor,
        execution_temp_root,
        cutover_manifest_hash,
        authority,
        planner,
        declared,
        Some(Arc::new(
            sigil_updater::ProductUpdaterState::from_cache_root(cache_root),
        )),
        execution_config,
        application_generation,
        Some(process_inventory),
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
    authority: AuthorityGeneration,
    planner: Arc<dyn ManagedExecutionPlannerV1>,
    declared: &[StorageWriterChannelV1],
    product_updater: Option<Arc<sigil_updater::ProductUpdaterState>>,
    execution_config: sigil_kernel::ExecutionConfig,
    application_generation: u64,
    process_inventory: Option<Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1>>,
) -> Result<RuntimeAuthorityCompositionV1, RuntimeAuthorityCompositionErrorV1> {
    // The real kernel capability broker is the single issuer for this composition: execution
    // bundles and storage admission capabilities are broker-issued (one-shot proofs), never
    // fabricated by consumers.
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
    };
    bootstrap
        .validate_anchors()
        .map_err(|error| RuntimeAuthorityCompositionErrorV1::AnchorInvalid(error.to_string()))?;

    let mut table = sigil_resource_authority::storage::AuthorityStorageGrantTableV1::new();
    for channel in declared {
        if *channel == crate::managed_storage_writer::StorageWriterChannelV1::DurableMemory {
            // Both DurableMemory scope classes are writer channels of the same family: the
            // ProjectFact grant covers the probe cell; the UserPreference grant keeps the
            // two-class DurableMemory writer from being partially closed.
            for grant in crate::managed_storage_writer::memory_grants_with_application_generation(
                0x76,
                authority,
                cutover_manifest_hash,
                application_generation,
            ) {
                table.register(grant).map_err(|error| {
                    RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
                })?;
            }
            continue;
        }
        let grant = crate::managed_storage_writer::grant_for_channel_with_application_generation(
            *channel,
            0x76,
            authority,
            cutover_manifest_hash,
            application_generation,
        );
        table.register(grant).map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
        })?;
    }
    let storage_service =
        sigil_resource_authority::storage::AuthorityManagedStorageServiceV1::new_with_state_root(
            table,
            authority,
            state_anchor,
        )
        .map_err(|error| {
            RuntimeAuthorityCompositionErrorV1::ExecutionConfigurationInvalid(error.to_string())
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
    let mut sandbox_execution = sigil_sandbox::managed::SandboxManagedExecutionServiceV1::new(
        Arc::clone(&planner),
        execution_temp_root.to_path_buf(),
    );
    if let Some(inventory) = &process_inventory {
        sandbox_execution = sandbox_execution.with_process_inventory(Arc::clone(inventory));
    }
    let execution: Arc<dyn sigil_kernel::managed_execution::ManagedExecutionServiceV1> =
        Arc::new(sandbox_execution);
    let mut extension_route = RuntimeManagedExtensionExecutionRouteV1::new(
        Arc::clone(&planner),
        Arc::clone(&broker),
        execution_temp_root.to_path_buf(),
    )
    .with_authority_generation(authority);
    if let Some(inventory) = &process_inventory {
        extension_route = extension_route.with_process_inventory(Arc::clone(inventory));
    }
    let extension_execution = std::sync::Arc::new(extension_route);
    let mut command_route = RuntimeManagedCommandExecutionRouteV1::new(
        Arc::clone(&planner),
        Arc::clone(&broker),
        execution_temp_root.to_path_buf(),
    );
    if let Some(inventory) = &process_inventory {
        command_route = command_route.with_process_inventory(Arc::clone(inventory));
    }
    let command_execution = std::sync::Arc::new(command_route);
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
    let artifact_staging_grant =
        crate::managed_storage_writer::grant_for_channel_with_application_generation(
            StorageWriterChannelV1::ArtifactStaging,
            0x76,
            authority,
            cutover_manifest_hash,
            application_generation,
        );
    let artifact_store_grant =
        crate::managed_storage_writer::grant_for_channel_with_application_generation(
            StorageWriterChannelV1::ArtifactStore,
            0x76,
            authority,
            cutover_manifest_hash,
            application_generation,
        );
    let artifact_retire_authority = std::sync::Arc::new(
        sigil_resource_authority::maintenance::ArtifactRetireAuthorityV1::new(
            authority,
            artifact_staging_grant.grant_hash,
            artifact_store_grant.grant_hash,
        ),
    );
    let storage_writer = std::sync::Arc::new(
        ManagedStorageWriterAdapterV1::with_storage_issuer_and_admission_context(
            storage,
            state_anchor.to_path_buf(),
            crate::managed_storage_writer::ManagedStorageAdmissionContextV1::application_cutover_root(
                cutover_manifest_hash,
                application_generation,
            )
            .map_err(|error| {
                RuntimeAuthorityCompositionErrorV1::GrantDeclared(error.to_string())
            })?,
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
    raw_config: &[u8],
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
    // The persisted bytes are the authority configuration identity. Parsed values may include
    // process-local model-request timeout overrides, which are session routing inputs and must
    // not advance the durable authority generation or invalidate a published cutover pointer.
    hasher.update(b"persisted-config\0");
    hasher.update(raw_config);
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

const AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION: u32 = 2;
const AUTHORITY_CONFIG_GENERATION_MIN_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct AuthorityConfigGenerationRecordV1 {
    schema_version: u32,
    config_hash: CanonicalHash,
    generation: u64,
    #[serde(default)]
    process_inventory_required: bool,
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
                sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(
                    "authority config generation metadata is missing".to_owned(),
                ),
            ));
        }
        return Ok(None);
    };
    if !(AUTHORITY_CONFIG_GENERATION_MIN_SCHEMA_VERSION
        ..=AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION)
        .contains(&record.schema_version)
        || record.generation == 0
        || (record.schema_version == AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION
            && !record.process_inventory_required)
    {
        return Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(
                "authority config generation record is invalid".to_owned(),
            ),
        ));
    }
    Ok(Some(record))
}

fn candidate_authority_config_generation(
    current: Option<AuthorityConfigGenerationRecordV1>,
    config_hash: CanonicalHash,
) -> Result<u64, BootAuthorityErrorV1> {
    let generation = match current.as_ref() {
        Some(record) if record.config_hash == config_hash => record.generation,
        Some(record) => record.generation.checked_add(1).ok_or_else(|| {
            BootAuthorityErrorV1::Config("authority config generation overflow".to_owned())
        })?,
        None => 1,
    };
    Ok(generation)
}

fn publish_authority_config_generation(
    bootstrap: &sigil_resource_authority::bootstrap::AuthorityBootstrapStoreV1,
    publication: &sigil_resource_authority::bootstrap::AuthorityBootstrapPublicationGuard,
    current: Option<AuthorityConfigGenerationRecordV1>,
    config_hash: CanonicalHash,
    generation: u64,
) -> Result<(), BootAuthorityErrorV1> {
    if current.as_ref().is_none_or(|record| {
        record.config_hash != config_hash
            || record.generation != generation
            || record.schema_version != AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION
            || !record.process_inventory_required
    }) {
        let record = AuthorityConfigGenerationRecordV1 {
            schema_version: AUTHORITY_CONFIG_GENERATION_SCHEMA_VERSION,
            config_hash,
            generation,
            process_inventory_required: true,
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
    Ok(())
}

/// Convenience: authoritative resource scope for the composition (application-level).
pub fn composition_authority_scope() -> ResourceAuthorityScopeV1 {
    ResourceAuthorityScopeV1::Application
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

/// Keeps startup blockers that need the durable recovery protocol distinct from malformed
/// bootstrap bytes. No inventory error is downgraded into an auto-repairable fresh bootstrap.
fn map_process_inventory_boot_error(
    error: sigil_resource_authority::AuthorityProcessInventoryErrorV1,
) -> BootAuthorityErrorV1 {
    use sigil_resource_authority::{
        AuthorityProcessInventoryErrorV1 as InventoryError, bootstrap::BootstrapErrorV1,
    };

    match error {
        InventoryError::Bootstrap(error) => BootAuthorityErrorV1::Bootstrap(error),
        error @ (InventoryError::InvalidBootstrapBinding
        | InventoryError::AuthenticatorUnavailable) => {
            BootAuthorityErrorV1::Config(error.to_string())
        }
        InventoryError::AuthenticatorCorrupted => {
            BootAuthorityErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(
                "authority process inventory authenticator is corrupted or mismatched".to_owned(),
            ))
        }
        error @ (InventoryError::LegacySchema
        | InventoryError::AuthenticatorMissing
        | InventoryError::CapacityExhausted
        | InventoryError::SequenceExhausted
        | InventoryError::InvalidClaim
        | InventoryError::PriorOwnerStillLive
        | InventoryError::PriorManagedProcessStillLive(_)
        | InventoryError::PreparedProcessRecoveryRequired
        | InventoryError::Observation(_)
        | InventoryError::LockPoisoned) => {
            BootAuthorityErrorV1::Bootstrap(BootstrapErrorV1::MetadataCorrupted(error.to_string()))
        }
    }
}

fn build_current_boot_transaction(
    config_snapshot: ValidatedAuthorityConfigSnapshotV1,
) -> Result<RuntimeCurrentBootTransactionV1, BootAuthorityErrorV1> {
    build_current_boot_transaction_inner(config_snapshot, false)
}

fn build_current_boot_transaction_with_source_guard(
    config_snapshot: ValidatedAuthorityConfigSnapshotV1,
) -> Result<RuntimeCurrentBootTransactionV1, BootAuthorityErrorV1> {
    build_current_boot_transaction_inner(config_snapshot, true)
}

fn build_current_boot_transaction_inner(
    config_snapshot: ValidatedAuthorityConfigSnapshotV1,
    require_source_stability: bool,
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
        .map_err(BootAuthorityErrorV1::Composition)?;
    cutover.gate().map_err(|error| {
        BootAuthorityErrorV1::Cutover(crate::r71_global_cutover::CutoverBootErrorV1::Guard(
            error.clone(),
        ))
    })?;
    if require_source_stability {
        ensure_config_snapshot_is_current(&config_snapshot)?;
    }
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

/// Reject an external editor replacing the reviewed config while authority was being composed.
/// Internal config writers already share the update lock; this final identity/source check also
/// protects the boot transaction from tools that do not know about Sigil's lock file.
fn ensure_config_snapshot_is_current(
    snapshot: &ValidatedAuthorityConfigSnapshotV1,
) -> Result<(), BootAuthorityErrorV1> {
    let Some((raw, identity)) = load_config_payload_with_identity(snapshot.config_path())? else {
        return Err(BootAuthorityErrorV1::Config(
            "authority config disappeared during boot; retry after restoring the reviewed file"
                .to_owned(),
        ));
    };
    let source_hash = sigil_process_observer::canonical_digest(&raw);
    if identity != snapshot.config_path_identity || source_hash != snapshot.source_hash() {
        return Err(BootAuthorityErrorV1::Config(
            "authority config changed during boot; the reviewed snapshot was not started; retry"
                .to_owned(),
        ));
    }
    Ok(())
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
    build_current_boot_transaction_with_source_guard(snapshot)
}

/// Boots the exact config reviewed by an interactive setup flow.
///
/// The update lock prevents Sigil's own writers from racing this boot. The persisted source is
/// also compared with the reviewed object before composition and once more before publication,
/// so an external atomic editor replacement becomes a visible retryable setup error instead of
/// silently starting a different workspace or authority root.
pub fn boot_current_schema_with_expected_config(
    config_path: &Path,
    launch_cwd: &Path,
    expected: &sigil_kernel::RootConfig,
) -> Result<RuntimeCurrentBootTransactionV1, BootAuthorityErrorV1> {
    let _config_lock = sigil_kernel::ConfigUpdateLockGuard::acquire(config_path)
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    let expected_source = expected
        .persisted_toml()
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?;
    let snapshot =
        ValidatedAuthorityConfigSnapshotV1::load(config_path, launch_cwd)?.ok_or_else(|| {
            BootAuthorityErrorV1::Config(
                "reviewed authority config is unavailable; return to setup and save again"
                    .to_owned(),
            )
        })?;
    if sigil_process_observer::canonical_digest(expected_source.as_bytes())
        != snapshot.source_hash()
    {
        return Err(BootAuthorityErrorV1::Config(
            "saved configuration changed before boot; the reviewed snapshot was not started; retry"
                .to_owned(),
        ));
    }
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
/// current authority composition have completed.
///
/// The readiness manifest is computed once against an isolated provisional composition, then the
/// production composition is rebound to the resulting content hash so storage grants and the
/// persisted cutover manifest carry the same source binding. Probes perform real admission and
/// settlement, so their provisional state must not contaminate the production state before the
/// source binding is frozen.
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
    let current_config_generation = load_authority_config_generation(bootstrap, publication)?;
    let application_generation = candidate_authority_config_generation(
        current_config_generation.clone(),
        config_snapshot.config_hash(),
    )?;
    let observer_binding = sigil_process_observer::canonical_digest(
        format!(
            "sigil-runtime-process-observer-v2\0{}\0{}\0{}",
            bootstrap.authority_epoch(),
            application_generation,
            config_snapshot.config_hash(),
        )
        .as_bytes(),
    );
    let process_factory = sigil_process_observer::ProcessObserverFactoryV1::new(observer_binding)
        .map_err(|error| BootAuthorityErrorV1::Config(error.to_string()))?
        .instantiate();
    let inventory_binding = sigil_resource_authority::AuthorityProcessInventoryBootstrapBindingV1 {
        application_composition_epoch: application_generation,
        owner_execution_scope_hash: sigil_process_observer::canonical_digest(
            format!(
                "sigil-runtime-process-inventory-owner-v2\0{}\0{}\0{}",
                bootstrap.authority_epoch(),
                application_generation,
                config_snapshot.config_hash(),
            )
            .as_bytes(),
        ),
    };
    let process_inventory: Arc<dyn sigil_resource_authority::AuthorityProcessInventoryPortV1> =
        Arc::new(
            sigil_resource_authority::AuthorityManagedProcessInventoryV1::initialize(
                bootstrap.clone(),
                publication,
                inventory_binding,
                process_factory,
            )
            .map_err(map_process_inventory_boot_error)?,
        );
    // The candidate is pure. Publish it only after the inventory's authenticated read/recovery
    // gate accepts the existing state, so malformed V1 inventory, a missing durable key, or a
    // bad MAC cannot be pre-emptively "repaired" by advancing configuration metadata.
    publish_authority_config_generation(
        bootstrap,
        publication,
        current_config_generation,
        config_snapshot.config_hash(),
        application_generation,
    )?;
    let authority = sigil_kernel::resource::AuthorityGeneration {
        epoch: bootstrap.authority_epoch(),
        instance_hash: bootstrap
            .authority_instance_hash(publication)
            .map_err(BootAuthorityErrorV1::Bootstrap)?,
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
    let provisional = compose_runtime_authority_with_product_updater_at_generation(
        &provisional_state_anchor,
        cache_root,
        execution_temp_root,
        config_snapshot,
        provisional_hash,
        application_generation,
        authority,
        planner,
        &declared,
        Arc::clone(&process_inventory),
    )
    .map_err(BootAuthorityErrorV1::Composition)?;
    let recovery = sigil_application::ApplicationResourceRecoveryFacadeV1::new();
    let first =
        crate::r71_global_cutover::RuntimeGlobalCutoverV1::evaluate_current_schema_with_source(
            instance_id.clone(),
            application_generation,
            authority,
            &provisional.services,
            &recovery,
            provisional_hash,
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
    let composition = match compose_runtime_authority_with_product_updater_at_generation(
        state_anchor,
        cache_root,
        execution_temp_root,
        config_snapshot,
        first.manifest().manifest_hash,
        application_generation,
        authority,
        planner,
        &declared,
        process_inventory,
    ) {
        Ok(composition) => composition,
        Err(error) => return Err(BootAuthorityErrorV1::Composition(error)),
    };
    let decision =
        crate::r71_global_cutover::RuntimeGlobalCutoverV1::evaluate_current_schema_with_source(
            instance_id,
            application_generation,
            authority,
            &composition.services,
            &recovery,
            first.manifest().manifest_hash,
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
                sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(
                    "authority config generation is missing before cutover publication".to_owned(),
                ),
            )
        })?;
    if generation.generation != decision.manifest().application_generation {
        return Err(BootAuthorityErrorV1::Bootstrap(
            sigil_resource_authority::bootstrap::BootstrapErrorV1::MetadataCorrupted(format!(
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
#[path = "tests/r71_authority_composition_tests.rs"]
mod tests;
