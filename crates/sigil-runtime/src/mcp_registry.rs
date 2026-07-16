use super::*;
use anyhow::bail;
use std::collections::BTreeMap;

use crate::mcp_declaration::declarations_by_effective_name;

/// Local/extension tool registry plus the request-context resolver that shares its code-intel
/// service instance.
pub struct RuntimeToolSurface {
    pub registry: ToolRegistry,
    pub context_resolver: crate::context::RequestContextResolver,
}

/// Read-through source for the latest durable plugin trust projection.
///
/// Implementations must load current state on every call. A cached discovery-time snapshot is not
/// sufficient because MCP approval and lazy activation may be separated from process spawn.
pub trait McpPluginTrustSource: Send + Sync {
    /// Rebuilds the latest plugin trust entries from the authoritative append-only source.
    fn current_plugin_trust(&self) -> Result<Vec<sigil_kernel::PluginTrustEntry>>;
}

/// Session-log-backed plugin trust source used at MCP activation boundaries.
#[derive(Clone)]
pub struct SessionMcpPluginTrustSource {
    session_log_path: PathBuf,
}

impl SessionMcpPluginTrustSource {
    #[must_use]
    pub fn new(session_log_path: impl Into<PathBuf>) -> Self {
        Self {
            session_log_path: session_log_path.into(),
        }
    }
}

impl std::fmt::Debug for SessionMcpPluginTrustSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionMcpPluginTrustSource")
            .field("session_log_path", &"[hidden]")
            .finish()
    }
}

impl McpPluginTrustSource for SessionMcpPluginTrustSource {
    fn current_plugin_trust(&self) -> Result<Vec<sigil_kernel::PluginTrustEntry>> {
        let entries = sigil_kernel::JsonlSessionStore::read_entries(&self.session_log_path)?;
        Ok(sigil_kernel::PluginStateProjection::from_entries(&entries)
            .trust_entries
            .into_values()
            .collect())
    }
}

/// Runtime controls for registering already-resolved MCP declarations.
pub struct McpDeclarationRegistrationOptions {
    startup: McpServerStartup,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    network_admission: ExtensionProcessNetworkAdmission,
    expected_process_subject: Option<ToolSubject>,
    plugin_trust_source: Option<Arc<dyn McpPluginTrustSource>>,
    strict_registration: bool,
}

impl McpDeclarationRegistrationOptions {
    #[must_use]
    pub fn new(startup: McpServerStartup) -> Self {
        Self {
            startup,
            elicitation_handler: sigil_mcp::unsupported_mcp_elicitation_handler(),
            runtime_event_handler: sigil_mcp::unsupported_mcp_runtime_event_handler(),
            mutation_recorder: None,
            network_admission: ExtensionProcessNetworkAdmission::default(),
            expected_process_subject: None,
            plugin_trust_source: None,
            strict_registration: false,
        }
    }

    #[must_use]
    pub fn with_handlers(
        mut self,
        elicitation_handler: Arc<dyn McpElicitationHandler>,
        runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    ) -> Self {
        self.elicitation_handler = elicitation_handler;
        self.runtime_event_handler = runtime_event_handler;
        self
    }

    #[must_use]
    pub fn with_mutation_recorder(mut self, recorder: MutationEventRecorder) -> Self {
        self.mutation_recorder = Some(recorder);
        self
    }

    #[must_use]
    pub fn with_network_admission(mut self, admission: ExtensionProcessNetworkAdmission) -> Self {
        self.network_admission = admission;
        self
    }

    #[must_use]
    pub fn with_expected_process_subject(mut self, subject: ToolSubject) -> Self {
        self.expected_process_subject = Some(subject);
        self
    }

    /// Installs a read-through source that is reloaded before authorization and before spawn.
    #[must_use]
    pub fn with_plugin_trust_source(mut self, source: Arc<dyn McpPluginTrustSource>) -> Self {
        self.plugin_trust_source = Some(source);
        self
    }

    /// Uses the append-only session log as the authoritative current plugin trust source.
    #[must_use]
    pub fn with_plugin_trust_session_log(self, session_log_path: impl Into<PathBuf>) -> Self {
        self.with_plugin_trust_source(Arc::new(SessionMcpPluginTrustSource::new(session_log_path)))
    }

    #[must_use]
    pub fn with_strict_registration(mut self) -> Self {
        self.strict_registration = true;
        self
    }
}

/// Registers MCP declarations without discarding origin, attestation or execution-base identity.
///
/// Plugin attestation and current trust are revalidated before process-subject/static-pin checks
/// and again immediately before spawn. The lower MCP transport receives only config plus the
/// declaration-aware launcher and safe lifecycle metadata.
///
/// # Errors
///
/// Returns an error when declaration identity is stale, command resolution fails, process
/// admission fails, or MCP initialization/tool registration fails.
pub async fn register_mcp_server_declarations(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    declarations: &[ResolvedMcpServerDeclaration],
    options: McpDeclarationRegistrationOptions,
) -> Result<McpToolRegistrationReport> {
    let stdio_declarations = declarations
        .iter()
        .filter(|declaration| declaration.config().stdio().is_some())
        .cloned()
        .collect::<Vec<_>>();
    if stdio_declarations.is_empty() {
        return Ok(McpToolRegistrationReport {
            process_launch_receipts: Vec::new(),
            lifecycle_owners: Vec::new(),
        });
    }
    let servers = stdio_declarations
        .iter()
        .map(|declaration| declaration.config().clone())
        .collect::<Vec<_>>();
    let mut registration_options =
        sigil_mcp::McpToolRegistrationOptions::for_startup(options.startup)?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
            .with_working_dir(workspace_root.clone())
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(options.elicitation_handler)
            .with_runtime_event_handler(options.runtime_event_handler)
            .with_pre_spawn_safe_metadata(declaration_pre_spawn_safe_metadata(&stdio_declarations))
            .with_process_launcher(declaration_mcp_process_launcher(
                root_config,
                &stdio_declarations,
                options.plugin_trust_source,
            )?)
            .with_network_admission(options.network_admission);
    if let Some(recorder) = options.mutation_recorder {
        registration_options =
            registration_options.with_mutation_recorder(workspace_root, recorder);
    }
    if let Some(subject) = options.expected_process_subject {
        registration_options = registration_options.with_expected_process_subject(subject);
    }
    if options.strict_registration {
        registration_options = registration_options.with_strict_registration();
    }
    sigil_mcp::register_mcp_tools_with_report(registry, &servers, registration_options).await
}

fn declaration_pre_spawn_safe_metadata(
    declarations: &[ResolvedMcpServerDeclaration],
) -> BTreeMap<String, BTreeMap<String, String>> {
    declarations
        .iter()
        .map(|declaration| {
            let projection = declaration.safe_projection();
            let mut metadata = BTreeMap::from([
                ("mcp_declared_name".to_owned(), projection.declared_name),
                ("mcp_effective_name".to_owned(), projection.effective_name),
                (
                    "mcp_config_origin".to_owned(),
                    projection.origin_kind.as_str().to_owned(),
                ),
                (
                    "mcp_execution_base_kind".to_owned(),
                    projection.execution_base_kind.as_str().to_owned(),
                ),
                (
                    "mcp_declaration_projection_fingerprint".to_owned(),
                    projection.declaration_fingerprint,
                ),
            ]);
            for (key, value) in [
                ("mcp_config_origin_id", projection.origin_id),
                ("mcp_manifest_hash", projection.manifest_hash),
                ("mcp_manifest_version", projection.manifest_version),
                ("mcp_capability_digest", projection.capability_digest),
                ("mcp_release_digest", projection.release_digest),
                (
                    "mcp_plugin_trust",
                    projection.trust.map(|trust| trust.as_str().to_owned()),
                ),
            ] {
                if let Some(value) = value {
                    metadata.insert(key.to_owned(), value);
                }
            }
            (declaration.effective_name().to_owned(), metadata)
        })
        .collect()
}

/// Builds the complete runtime tool registry from built-ins and configured MCP servers.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried.
pub async fn build_tool_registry(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mcp_elicitation(
        root_config,
        provider_capabilities,
        workspace_root,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await
}

/// Builds the complete runtime tool registry while durably recording eager MCP process lifecycle
/// evidence into the active session store.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried, when lifecycle
/// evidence cannot be appended, or when local tool construction fails.
pub async fn build_tool_registry_with_mutation_recorder(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    mutation_recorder: MutationEventRecorder,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mutation_recorder_and_workspace_trust(
        root_config,
        provider_capabilities,
        workspace_root,
        mutation_recorder,
        WorkspaceTrust::Unknown,
    )
    .await
}

/// Builds the complete runtime tool registry with an explicit durable workspace-trust projection.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried, when lifecycle
/// evidence cannot be appended, or when local tool construction fails.
pub async fn build_tool_registry_with_mutation_recorder_and_workspace_trust(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    mutation_recorder: MutationEventRecorder,
    workspace_trust: WorkspaceTrust,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mutation_recorder_and_workspace_trust_and_network_admission(
        root_config,
        provider_capabilities,
        workspace_root,
        mutation_recorder,
        workspace_trust,
        ExtensionProcessNetworkAdmission::default(),
    )
    .await
}

/// Builds the complete runtime tool registry with explicit process network admission.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried, when lifecycle
/// evidence cannot be appended, or when the process network admission cannot be enforced.
#[allow(clippy::too_many_arguments)]
pub async fn build_tool_registry_with_mutation_recorder_and_workspace_trust_and_network_admission(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    mutation_recorder: MutationEventRecorder,
    workspace_trust: WorkspaceTrust,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<ToolRegistry> {
    Ok(build_tool_surface_with_mcp_handlers_and_mutation_recorder(
        root_config,
        provider_capabilities,
        workspace_root,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        Some(mutation_recorder),
        workspace_trust,
        network_admission,
    )
    .await?
    .registry)
}

/// Builds the complete runtime tool and Context V1 surface with explicit process network
/// admission.
///
/// The returned resolver owns a clone of the exact `CodeIntelligenceService` registered behind
/// the code-intelligence tools, so request assembly can only inspect that service's warm cache.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`build_tool_registry_with_mutation_recorder_and_workspace_trust_and_network_admission`].
#[allow(clippy::too_many_arguments)]
pub async fn build_tool_surface_with_mutation_recorder_and_workspace_trust_and_network_admission(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    mutation_recorder: MutationEventRecorder,
    workspace_trust: WorkspaceTrust,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<RuntimeToolSurface> {
    build_tool_surface_with_mcp_handlers_and_mutation_recorder(
        root_config,
        provider_capabilities,
        workspace_root,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
        Some(mutation_recorder),
        workspace_trust,
        network_admission,
    )
    .await
}

/// Builds the runtime tool registry using a caller-provided MCP elicitation handler.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried.
pub async fn build_tool_registry_with_mcp_elicitation(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mcp_handlers(
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
}

/// Builds the runtime tool registry using caller-provided MCP handlers.
///
/// # Errors
///
/// Returns an error when one configured MCP server cannot be started or queried.
pub async fn build_tool_registry_with_mcp_handlers(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<ToolRegistry> {
    build_tool_registry_with_mcp_handlers_and_mutation_recorder(
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
        None,
        WorkspaceTrust::Unknown,
        ExtensionProcessNetworkAdmission::default(),
    )
    .await
}

async fn build_tool_registry_with_mcp_handlers_and_mutation_recorder(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    workspace_trust: WorkspaceTrust,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<ToolRegistry> {
    Ok(build_tool_surface_with_mcp_handlers_and_mutation_recorder(
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
        mutation_recorder,
        workspace_trust,
        network_admission,
    )
    .await?
    .registry)
}

#[allow(clippy::too_many_arguments)]
async fn build_tool_surface_with_mcp_handlers_and_mutation_recorder(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    workspace_trust: WorkspaceTrust,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<RuntimeToolSurface> {
    let declarations =
        resolve_user_root_mcp_declarations(&root_config.mcp_servers, &workspace_root)?;
    let mut registry = ToolRegistry::new();
    let code_intelligence = register_local_tools(
        &mut registry,
        root_config,
        workspace_root.clone(),
        workspace_trust,
    )?;
    let context_resolver =
        crate::context::RequestContextResolver::new(workspace_root.clone(), code_intelligence);
    let mut registration_options = McpDeclarationRegistrationOptions::new(McpServerStartup::Eager)
        .with_handlers(
            Arc::clone(&elicitation_handler),
            Arc::clone(&runtime_event_handler),
        )
        .with_network_admission(network_admission);
    if let Some(recorder) = mutation_recorder {
        registration_options = registration_options.with_mutation_recorder(recorder);
    }
    register_mcp_server_declarations(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root.clone(),
        &declarations,
        registration_options,
    )
    .await?;
    register_lazy_mcp_activation_tool(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    );
    Ok(RuntimeToolSurface {
        registry,
        context_resolver,
    })
}

/// Builds the local tool surface and lazy MCP activation tool without starting eager MCP servers.
///
/// TUI entrypoints use this to keep the agent worker available when an external MCP server is
/// slow or broken. Eager MCP servers can then be activated asynchronously against the returned
/// shared registry.
///
/// # Errors
///
/// Returns an error when local tool construction fails, including execution backend policies that
/// cannot be satisfied by the configured backend.
pub fn build_tool_registry_without_eager_mcp(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<ToolRegistry> {
    build_tool_registry_without_eager_mcp_with_workspace_trust(
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
        WorkspaceTrust::Unknown,
    )
}

/// Builds the local tool surface with an explicit durable workspace-trust projection.
///
/// TUI entrypoints use this after loading the active session so language-server process admission
/// can fail closed without disabling local Tree-sitter fallback tools.
///
/// # Errors
///
/// Returns an error when local tool construction fails, including execution backend policies that
/// cannot be satisfied by the configured backend.
pub fn build_tool_registry_without_eager_mcp_with_workspace_trust(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    workspace_trust: WorkspaceTrust,
) -> Result<ToolRegistry> {
    Ok(build_tool_surface_without_eager_mcp_with_workspace_trust(
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
        workspace_trust,
    )?
    .registry)
}

/// Builds the local tool and Context V1 surface with an explicit workspace-trust projection.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`build_tool_registry_without_eager_mcp_with_workspace_trust`].
pub fn build_tool_surface_without_eager_mcp_with_workspace_trust(
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    workspace_trust: WorkspaceTrust,
) -> Result<RuntimeToolSurface> {
    let _declarations =
        resolve_user_root_mcp_declarations(&root_config.mcp_servers, &workspace_root)?;
    let mut registry = ToolRegistry::new();
    let code_intelligence = register_local_tools(
        &mut registry,
        root_config,
        workspace_root.clone(),
        workspace_trust,
    )?;
    let context_resolver =
        crate::context::RequestContextResolver::new(workspace_root.clone(), code_intelligence);
    register_lazy_mcp_activation_tool(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    );
    Ok(RuntimeToolSurface {
        registry,
        context_resolver,
    })
}

/// Activates lazy MCP servers against an existing runtime tool registry.
///
/// Returns the number of tools added to the registry. When `server_name` is set, only the
/// matching lazy server is activated.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
) -> Result<usize> {
    Ok(activate_lazy_mcp_tools_detailed(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
    )
    .await?
    .added_tools)
}

/// Detailed result for one lazy MCP activation attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LazyMcpActivationResult {
    pub matched_servers: usize,
    pub added_tools: usize,
    pub process_launch_receipts: Vec<McpProcessLaunchReceipt>,
}

/// Activates lazy MCP servers and reports both matched server and added tool counts.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_elicitation(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        sigil_mcp::unsupported_mcp_elicitation_handler(),
    )
    .await
}

/// Activates lazy MCP servers using a caller-provided MCP elicitation handler.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_elicitation(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_handlers(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        sigil_mcp::unsupported_mcp_runtime_event_handler(),
    )
    .await
}

/// Activates lazy MCP servers using caller-provided MCP handlers.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_handlers(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        None,
    )
    .await
}

/// Activates lazy MCP servers while recording conservative external-process mutation evidence.
///
/// # Errors
///
/// Returns an error when a required lazy MCP server cannot be started, initialized, or queried.
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder_and_network_admission(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        mutation_recorder,
        ExtensionProcessNetworkAdmission::default(),
    )
    .await
}

/// Activates lazy MCP servers with explicit process network admission.
///
/// # Errors
///
/// Returns an error when a selected server cannot be started, initialized, queried, or admitted by
/// the run-scoped network policy.
#[allow(clippy::too_many_arguments)]
pub async fn activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder_and_network_admission(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<LazyMcpActivationResult> {
    activate_lazy_mcp_tools_detailed_inner(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        mutation_recorder,
        None,
        network_admission,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn activate_lazy_mcp_tools_detailed_inner(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: Option<&str>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    expected_process_subject: Option<ToolSubject>,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<LazyMcpActivationResult> {
    let declarations =
        resolve_user_root_mcp_declarations(&root_config.mcp_servers, &workspace_root)?;
    let selected_declarations = declarations
        .iter()
        .filter(|declaration| declaration.config().startup == McpServerStartup::Lazy)
        .filter(|declaration| server_name.is_none_or(|name| declaration.effective_name() == name))
        .cloned()
        .collect::<Vec<_>>();
    if selected_declarations.is_empty() {
        return Ok(LazyMcpActivationResult {
            matched_servers: 0,
            added_tools: 0,
            process_launch_receipts: Vec::new(),
        });
    }
    let before = registry.specs().len();
    let mut registration_options = McpDeclarationRegistrationOptions::new(McpServerStartup::Lazy)
        .with_handlers(elicitation_handler, runtime_event_handler)
        .with_network_admission(network_admission);
    if let Some(recorder) = mutation_recorder {
        registration_options = registration_options.with_mutation_recorder(recorder);
    }
    if let Some(subject) = expected_process_subject {
        registration_options = registration_options.with_expected_process_subject(subject);
    }
    if server_name.is_some() {
        registration_options = registration_options.with_strict_registration();
    }
    let report = register_mcp_server_declarations(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        &selected_declarations,
        registration_options,
    )
    .await?;
    Ok(LazyMcpActivationResult {
        matched_servers: selected_declarations.len(),
        added_tools: registry.specs().len().saturating_sub(before),
        process_launch_receipts: report.process_launch_receipts,
    })
}

/// Detailed result for one MCP server refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRefreshResult {
    pub matched_servers: usize,
    pub removed_tools: usize,
    pub added_tools: usize,
    pub process_launch_receipts: Vec<McpProcessLaunchReceipt>,
}

fn register_local_tools(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    workspace_root: PathBuf,
    workspace_trust: WorkspaceTrust,
) -> Result<Option<sigil_code_intel::CodeIntelligenceService>> {
    let paths = resolve_sigil_paths(&root_config.storage, &root_config.session, &workspace_root);
    let execution_backend = build_configured_execution_backend(root_config)?;
    sigil_tools_builtin::register_builtin_tools_with_paths_execution_backend_and_execution_config(
        registry,
        sigil_tools_builtin::BuiltinToolPaths {
            changesets_root: paths.changesets_root.clone(),
            changesets_label_root: PathBuf::from("state/artifacts/changesets"),
            terminal_tasks_root: paths.terminal_tasks_root.clone(),
            terminal_tasks_label_root: PathBuf::from("state/artifacts/tasks"),
            scratch_root: paths.scratch_root.clone(),
            scratch_label: "cache/tmp".to_owned(),
        },
        execution_backend,
        &root_config.execution,
    );
    let code_intelligence = sigil_code_intel::register_code_intelligence_tools_with_workspace_trust(
        registry,
        &root_config.code_intelligence,
        workspace_root.clone(),
        workspace_trust,
    );
    let user_config_dir = default_user_config_dir().ok();
    let _ = skills::register_skill_tools(
        registry,
        &workspace_root,
        user_config_dir.as_deref(),
        &root_config.skills,
    );
    Ok(code_intelligence)
}

/// Builds the execution backend configured for tools and verification checks.
///
/// # Errors
///
/// Returns an error when the configured backend cannot satisfy the requested isolation policy.
pub fn build_configured_execution_backend(
    root_config: &RootConfig,
) -> Result<Arc<dyn ExecutionBackend>> {
    sigil_tools_builtin::build_execution_backend(&root_config.execution)
}

fn declaration_mcp_process_launcher(
    root_config: &RootConfig,
    declarations: &[ResolvedMcpServerDeclaration],
    plugin_trust_source: Option<Arc<dyn McpPluginTrustSource>>,
) -> Result<Arc<dyn McpProcessLauncher>> {
    if plugin_trust_source.is_none()
        && let Some(declaration) = declarations.iter().find(|declaration| {
            matches!(declaration.origin(), McpConfigOrigin::PluginManifest { .. })
        })
    {
        return Err(McpRegistrationError {
            code: McpRegistrationErrorCode::PluginAttestationReviewRequired,
            declared_name: declaration.declared_name().to_owned(),
            reason: "a live plugin trust source is required for MCP activation".to_owned(),
            safe_projection: Some(Box::new(declaration.safe_projection())),
        }
        .into());
    }
    Ok(Arc::new(DeclarationAwareMcpProcessLauncher {
        configured: ConfiguredMcpProcessLauncher {
            execution: root_config.execution.clone(),
        },
        declarations: declarations_by_effective_name(declarations)?,
        plugin_trust_source,
    }))
}

#[derive(Clone)]
struct DeclarationAwareMcpProcessLauncher {
    configured: ConfiguredMcpProcessLauncher,
    declarations: BTreeMap<String, ResolvedMcpServerDeclaration>,
    plugin_trust_source: Option<Arc<dyn McpPluginTrustSource>>,
}

impl std::fmt::Debug for DeclarationAwareMcpProcessLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeclarationAwareMcpProcessLauncher")
            .field("configured", &self.configured)
            .field("declarations", &self.declarations)
            .field("plugin_trust_source", &self.plugin_trust_source.is_some())
            .finish()
    }
}

impl DeclarationAwareMcpProcessLauncher {
    fn current_plugin_trust(
        &self,
        declaration: &ResolvedMcpServerDeclaration,
    ) -> Result<Vec<sigil_kernel::PluginTrustEntry>> {
        if !matches!(declaration.origin(), McpConfigOrigin::PluginManifest { .. }) {
            return Ok(Vec::new());
        }
        let source = self
            .plugin_trust_source
            .as_ref()
            .ok_or_else(|| McpRegistrationError {
                code: McpRegistrationErrorCode::PluginAttestationReviewRequired,
                declared_name: declaration.declared_name().to_owned(),
                reason: "a live plugin trust source is unavailable at activation".to_owned(),
                safe_projection: Some(Box::new(declaration.safe_projection())),
            })?;
        source.current_plugin_trust().map_err(|_| {
            McpRegistrationError {
                code: McpRegistrationErrorCode::PluginAttestationReviewRequired,
                declared_name: declaration.declared_name().to_owned(),
                reason: "current plugin trust could not be reloaded before activation".to_owned(),
                safe_projection: Some(Box::new(declaration.safe_projection())),
            }
            .into()
        })
    }
}

impl McpProcessLauncher for DeclarationAwareMcpProcessLauncher {
    fn resolve_launch_request(
        &self,
        config: &McpServerConfig,
        _fallback_working_dir: Option<PathBuf>,
    ) -> Result<McpProcessLaunchRequest> {
        let declaration = self.declarations.get(&config.name).ok_or_else(|| {
            anyhow!(
                "missing resolved MCP declaration for effective server {}",
                config.name
            )
        })?;
        if config != declaration.config() {
            return Err(McpRegistrationError {
                code: McpRegistrationErrorCode::McpDeclarationBindingChanged,
                declared_name: declaration.declared_name().to_owned(),
                reason: "MCP config drifted after declaration resolution".to_owned(),
                safe_projection: Some(Box::new(declaration.safe_projection())),
            }
            .into());
        }
        let current_plugin_trust = self.current_plugin_trust(declaration)?;
        let launch = declaration
            .resolve_stdio_launch(&current_plugin_trust)
            .map_err(|error| error.with_safe_projection(declaration.safe_projection()))?;
        let mut request = McpProcessLaunchRequest::from_config(config, Some(launch.cwd.clone()))?;
        if declaration.uses_declaration_static_binding() {
            let projection = declaration.safe_projection();
            request.launch_static_fingerprint =
                sigil_mcp::mcp_resolved_launch_static_fingerprint_at(
                    &projection.declaration_fingerprint,
                    &launch.executable,
                )?;
        }
        request.command = launch.executable.to_string_lossy().into_owned();
        request.working_dir = Some(launch.cwd.clone());
        request.classification = launch.classification;
        request.declaration = Some(declaration.launch_metadata(
            &launch,
            &request.launch_static_fingerprint,
            request.environment.live_fingerprint(),
        ));
        Ok(request)
    }

    fn launch(&self, mut request: McpProcessLaunchRequest) -> Result<McpProcessLaunch> {
        let declaration = self.declarations.get(&request.server_name).ok_or_else(|| {
            anyhow!(
                "missing resolved MCP declaration for effective server {}",
                request.server_name
            )
        })?;
        let declaration_args = declaration
            .config()
            .stdio()
            .map(|(_, args, _)| args)
            .ok_or_else(|| anyhow!("remote MCP declaration cannot use stdio process launcher"))?;
        if request.args != declaration_args {
            return Err(McpRegistrationError {
                code: McpRegistrationErrorCode::McpDeclarationBindingChanged,
                declared_name: declaration.declared_name().to_owned(),
                reason: "MCP launch args drifted after declaration resolution".to_owned(),
                safe_projection: Some(Box::new(declaration.safe_projection())),
            }
            .into());
        }
        let current_plugin_trust = self.current_plugin_trust(declaration)?;
        let launch = declaration
            .resolve_stdio_launch(&current_plugin_trust)
            .map_err(|error| error.with_safe_projection(declaration.safe_projection()))?;
        let fresh_static_fingerprint = if declaration.uses_declaration_static_binding() {
            let projection = declaration.safe_projection();
            sigil_mcp::mcp_resolved_launch_static_fingerprint_at(
                &projection.declaration_fingerprint,
                &launch.executable,
            )?
        } else {
            request.launch_static_fingerprint.clone()
        };
        if fresh_static_fingerprint != request.launch_static_fingerprint {
            return Err(McpRegistrationError {
                code: McpRegistrationErrorCode::McpDeclarationBindingChanged,
                declared_name: declaration.declared_name().to_owned(),
                reason: "MCP executable identity changed after authorization".to_owned(),
                safe_projection: Some(Box::new(declaration.safe_projection())),
            }
            .into());
        }
        let fresh_metadata = declaration.launch_metadata(
            &launch,
            &request.launch_static_fingerprint,
            request.environment.live_fingerprint(),
        );
        if request.declaration.as_ref() != Some(&fresh_metadata) {
            return Err(McpRegistrationError {
                code: McpRegistrationErrorCode::McpDeclarationBindingChanged,
                declared_name: declaration.declared_name().to_owned(),
                reason: "MCP declaration or resolved process binding changed after authorization"
                    .to_owned(),
                safe_projection: Some(Box::new(declaration.safe_projection())),
            }
            .into());
        }
        request.command = launch.executable.to_string_lossy().into_owned();
        request.working_dir = Some(launch.cwd);
        request.classification = launch.classification;
        self.configured.launch(request)
    }
}

#[derive(Debug, Clone)]
pub(super) struct ConfiguredMcpProcessLauncher {
    pub(super) execution: sigil_kernel::ExecutionConfig,
}

impl McpProcessLauncher for ConfiguredMcpProcessLauncher {
    fn launch(&self, request: McpProcessLaunchRequest) -> Result<McpProcessLaunch> {
        let cwd = request
            .working_dir
            .clone()
            .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        let plan = sigil_tools_builtin::long_lived_stdio_process_plan(
            &self.execution,
            &request.command,
            &request.args,
            &cwd,
            &request.environment,
        )?;
        launch_planned_mcp_process(request, plan)
    }
}

pub(super) fn launch_planned_mcp_process(
    request: McpProcessLaunchRequest,
    plan: sigil_tools_builtin::LongLivedStdioProcessPlan,
) -> Result<McpProcessLaunch> {
    sigil_kernel::validate_extension_process_network_admission(
        plan.sandbox_profile,
        Some(NetworkEffect::Unknown),
        request.network_admission,
        plan.backend_capabilities,
        &plan.network,
        format!("mcp_server:{}", request.server_name),
    )?;
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .current_dir(&plan.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command.env_clear();
    for (name, value) in plan.environment.variables() {
        command.env(name, value.expose_secret());
    }
    let child = command
        .spawn()
        .with_context(|| format!("failed to spawn MCP server {}", request.server_name))?;
    let coverage = if plan.sandboxed {
        sigil_mcp::McpProcessCoverage::LocalStdioSandboxed
    } else {
        sigil_mcp::McpProcessCoverage::LocalStdioOutsideSandbox
    };
    let classification = if plan.sandboxed {
        sigil_mcp::McpProcessClass::LocalStdioSandboxed
    } else {
        request.classification
    };
    Ok(McpProcessLaunch {
        child,
        receipt: McpProcessLaunchReceipt {
            server_name: request.server_name,
            classification,
            coverage,
            backend: Some(plan.backend),
            backend_capabilities: Some(plan.backend_capabilities),
            sandbox_profile: Some(plan.sandbox_profile),
            network: plan.network,
            environment_policy: plan.environment.policy(),
            environment_baseline_names: plan.environment.baseline_names().to_vec(),
            environment_grant_names: plan.environment.grant_names().to_vec(),
            environment_static_fingerprint: plan.environment.static_fingerprint().to_owned(),
            environment_live_fingerprint: plan.environment.live_fingerprint().to_owned(),
            launch_static_fingerprint: request.launch_static_fingerprint,
            declaration: request.declaration,
        },
    })
}

/// Refreshes provider-visible tools for one configured MCP server.
///
/// # Errors
///
/// Returns an error when a required MCP server cannot be restarted, initialized, or queried.
pub async fn refresh_mcp_server_tools_with_mcp_handlers(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: &str,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) -> Result<McpRefreshResult> {
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        None,
    )
    .await
}

/// Refreshes one MCP server while recording conservative external-process mutation evidence.
///
/// # Errors
///
/// Returns an error when a required MCP server cannot be restarted, initialized, or queried.
pub async fn refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: &str,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
) -> Result<McpRefreshResult> {
    refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder_and_network_admission(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        mutation_recorder,
        ExtensionProcessNetworkAdmission::default(),
    )
    .await
}

/// Refreshes one MCP server with explicit process network admission.
///
/// # Errors
///
/// Returns an error when the server cannot be restarted, initialized, queried, or admitted by the
/// run-scoped network policy.
#[allow(clippy::too_many_arguments)]
pub async fn refresh_mcp_server_tools_with_mcp_handlers_and_mutation_recorder_and_network_admission(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: &str,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<McpRefreshResult> {
    refresh_mcp_server_tools_inner(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        server_name,
        elicitation_handler,
        runtime_event_handler,
        mutation_recorder,
        network_admission,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn refresh_mcp_server_tools_inner(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    server_name: &str,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    mutation_recorder: Option<MutationEventRecorder>,
    network_admission: ExtensionProcessNetworkAdmission,
) -> Result<McpRefreshResult> {
    let declarations =
        resolve_user_root_mcp_declarations(&root_config.mcp_servers, &workspace_root)?;
    let selected_declarations = declarations
        .iter()
        .filter(|declaration| declaration.effective_name() == server_name)
        .cloned()
        .collect::<Vec<_>>();
    let Some(declaration) = selected_declarations.first() else {
        return Ok(McpRefreshResult {
            matched_servers: 0,
            removed_tools: 0,
            added_tools: 0,
            process_launch_receipts: Vec::new(),
        });
    };
    let server = declaration.config();
    let retired_owners =
        registry.lifecycle_owners_by_scope(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, server_name);
    let removed = retired_owners
        .iter()
        .flat_map(|owner| registry.drain_by_lifecycle_owner(owner))
        .collect::<Vec<_>>();
    let removed_tools = removed.len();
    let before = registry.specs().len();
    let mut registration_options = McpDeclarationRegistrationOptions::new(server.startup)
        .with_handlers(elicitation_handler, runtime_event_handler)
        .with_network_admission(network_admission)
        .with_strict_registration();
    if let Some(recorder) = mutation_recorder {
        registration_options = registration_options.with_mutation_recorder(recorder);
    }
    let report = match register_mcp_server_declarations(
        registry,
        root_config,
        provider_capabilities,
        workspace_root,
        &selected_declarations,
        registration_options,
    )
    .await
    {
        Ok(report) => report,
        Err(error) => {
            for tool in removed {
                registry.register(tool);
            }
            return Err(error);
        }
    };
    if let Err(retirement_error) = shutdown_registered_tools(&removed).await {
        let replacement = report
            .lifecycle_owners
            .iter()
            .flat_map(|owner| registry.drain_by_lifecycle_owner(owner))
            .collect::<Vec<_>>();
        let rollback_error = shutdown_registered_tools(&replacement).await.err();
        return Err(anyhow!(
            "failed to retire previous MCP server {server_name} generation: {retirement_error:#}{}",
            rollback_error
                .map(|error| format!("; replacement generation rollback was incomplete: {error:#}"))
                .unwrap_or_default()
        ));
    }
    Ok(McpRefreshResult {
        matched_servers: selected_declarations.len(),
        removed_tools,
        added_tools: registry.specs().len().saturating_sub(before),
        process_launch_receipts: report.process_launch_receipts,
    })
}

pub(super) async fn shutdown_registered_tools(tools: &[Arc<dyn Tool>]) -> Result<()> {
    let mut attempted_owners = Vec::new();
    let mut failures = Vec::new();
    for tool in tools {
        if let Some(owner) = tool.lifecycle_owner() {
            if attempted_owners.contains(&owner) {
                continue;
            }
            attempted_owners.push(owner);
        }
        if let Err(error) = tool.shutdown().await {
            failures.push(format!(
                "failed to shut down registered tool {}: {error:#}",
                tool.spec().name
            ));
        }
    }
    if !failures.is_empty() {
        bail!(failures.join("; "));
    }
    Ok(())
}

pub(super) fn register_lazy_mcp_activation_tool(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) {
    if !root_config.mcp_servers.iter().any(|server| {
        server.startup == McpServerStartup::Lazy || server.streamable_http().is_some()
    }) {
        return;
    }
    registry.register(Arc::new(McpActivateServerTool {
        registry: registry.downgrade(),
        root_config: root_config.clone(),
        provider_capabilities: provider_capabilities.clone(),
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
        remote_presenter: None,
    }));
}

/// Replaces the default fail-closed activation tool with one bound to a concrete product-surface
/// disclosure presenter. This is required before a user-root Streamable HTTP server can connect.
pub fn attach_remote_mcp_activation_presenter(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    presenter: Arc<dyn sigil_kernel::EgressDisclosurePresenter>,
) {
    crate::web_fetch_tool::register_web_fetch_tool(registry, root_config, Arc::clone(&presenter));
    crate::web_search_tool::register_web_search_tool(
        registry,
        root_config,
        provider_capabilities.tool_name_max_chars,
        Arc::clone(&presenter),
    );
    registry.set_run_input_preparer(Arc::new(
        crate::hosted_web_search::HostedWebSearchInputPreparer::new(
            root_config.clone(),
            Arc::clone(&presenter),
        ),
    ));
    if !root_config
        .mcp_servers
        .iter()
        .any(|server| server.streamable_http().is_some())
    {
        return;
    }
    registry.register(Arc::new(McpActivateServerTool {
        registry: registry.downgrade(),
        root_config: root_config.clone(),
        provider_capabilities: provider_capabilities.clone(),
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
        remote_presenter: Some(presenter),
    }));
}

#[derive(Clone)]
struct McpActivateServerTool {
    registry: sigil_kernel::WeakToolRegistry,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    remote_presenter: Option<Arc<dyn sigil_kernel::EgressDisclosurePresenter>>,
}

fn mcp_activation_tool_spec() -> ToolSpec {
    ToolSpec {
        name: "mcp_activate_server".to_owned(),
        description: "Activate a configured lazy MCP server so its real tools become available on the next model turn."
            .to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "server_name": {
                    "type": "string",
                    "description": "Name of the configured MCP server with startup = lazy."
                }
            },
            "required": ["server_name"]
        }),
        category: ToolCategory::Mcp,
        access: ToolAccess::Execute,
        network_effect: Some(NetworkEffect::Unknown),
        preview: ToolPreviewCapability::None,
    }
}

#[async_trait]
impl Tool for McpActivateServerTool {
    fn spec(&self) -> ToolSpec {
        mcp_activation_tool_spec()
    }

    fn mutation_tracking(&self) -> sigil_kernel::ToolMutationTracking {
        // Local stdio activation records its own pre/post-start lifecycle scan in sigil-mcp,
        // while remote activation cannot mutate the workspace. A second generic unknown-mutation
        // scan would not add evidence and prevents the stable websearch wrapper from activating
        // its already-approved configured binding.
        sigil_kernel::ToolMutationTracking::None
    }

    fn permission_operation(&self, _ctx: &ToolContext, _args: &Value) -> Result<ToolOperation> {
        Ok(ToolOperation::NetworkRequest)
    }

    fn permission_access(&self, _ctx: &ToolContext, args: &Value) -> Result<ToolAccess> {
        let server_name = required_server_name(args)?;
        Ok(self
            .lazy_server(server_name)
            .filter(|server| server.streamable_http().is_some())
            .map_or(ToolAccess::Execute, |_| ToolAccess::Read))
    }

    fn permission_network_effect(
        &self,
        ctx: &ToolContext,
        args: &Value,
    ) -> Result<Option<NetworkEffect>> {
        let server_name = required_server_name(args)?;
        let Some(server) = self.lazy_server(server_name) else {
            return Ok(Some(NetworkEffect::Unknown));
        };
        Ok(mcp_server_process_network_effect(
            &self.root_config.execution,
            &self.workspace_root,
            server,
            ctx.network_policy(),
        ))
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let server_name = required_server_name(args)?;
        let Some(server) = self.lazy_server(server_name) else {
            return Ok(vec![mcp_server_subject(server_name)]);
        };
        mcp_server_process_subjects(server, &self.workspace_root)
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        let server_name = required_server_name(args)?;
        Ok(self
            .lazy_server(server_name)
            .map(|server| server.trust.approval_default))
    }

    fn egress_audit(&self, _ctx: &ToolContext, args: &Value) -> Result<Option<ToolEgressAudit>> {
        let server_name = required_server_name(args)?;
        let Some(server) = self.lazy_server(server_name) else {
            return Ok(None);
        };
        if !server.trust.egress_logging {
            return Ok(None);
        }
        let Some((_, _, inherit_env)) = server.stdio() else {
            let remote = server
                .streamable_http()
                .expect("non-stdio MCP server is streamable HTTP");
            return Ok(Some(ToolEgressAudit {
                destination: format!("mcp:{server_name}"),
                operation: "server/activate".to_owned(),
                payload: json!({
                    "server": server_name,
                    "transport": "streamable_http",
                    "safe_destination": crate::mcp_stdio_boundary_summary(
                        &self.root_config,
                        &self.workspace_root,
                        server,
                    ),
                    "header_names": remote
                        .http_headers
                        .keys()
                        .chain(remote.env_http_headers.keys())
                        .cloned()
                        .collect::<Vec<_>>(),
                    "credential_environment_names": remote
                        .env_http_headers
                        .values()
                        .chain(remote.bearer_token_env_var.iter())
                        .cloned()
                        .collect::<Vec<_>>(),
                }),
                redacted: true,
            }));
        };
        let environment = sigil_kernel::resolve_extension_process_environment(inherit_env)?;
        let (_, declaration) = user_root_mcp_launch_binding(server, &self.workspace_root)?;
        Ok(Some(ToolEgressAudit {
            destination: format!("mcp:{server_name}"),
            operation: "server/activate".to_owned(),
            payload: json!({
                "server": server_name,
                "trust_class": server.trust.trust_class.as_str(),
                "startup": server.startup.as_str(),
                "environment_grant_names": environment.grant_names(),
                "environment_grant_source": "parent_environment",
                "environment_static_fingerprint": environment.static_fingerprint(),
                "environment_live_fingerprint": environment.live_fingerprint(),
                "launch_static_fingerprint": declaration.transport_static_fingerprint,
                "declaration": declaration.metadata,
            }),
            redacted: false,
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let server_name = required_server_name(&args)?;
        if self.lazy_server(server_name).is_none() {
            return Ok(ToolResult::error(
                call_id,
                "mcp_activate_server",
                ToolErrorKind::InvalidInput,
                format!("unknown lazy MCP server {server_name}"),
            ));
        }
        if self.registered_tool_count(server_name) > 0 {
            return Ok(activation_result(
                call_id,
                server_name,
                "already_ready",
                1,
                0,
                &[],
            ));
        }

        let mut registry = self
            .registry
            .upgrade()
            .ok_or_else(|| anyhow!("MCP tool registry is no longer available"))?;
        if let Some(server) = self.lazy_server(server_name)
            && server.streamable_http().is_some()
        {
            let presenter = self.remote_presenter.clone().ok_or_else(|| {
                anyhow!("remote MCP activation requires a concrete disclosure presenter")
            })?;
            let added_tools = crate::activate_remote_mcp_server(
                &mut registry,
                &self.root_config,
                server,
                self.provider_capabilities.tool_name_max_chars,
                &ctx,
                presenter,
                Arc::clone(&self.elicitation_handler),
            )
            .await?;
            return Ok(activation_result(
                call_id,
                server_name,
                "ready",
                1,
                added_tools,
                &[],
            ));
        }
        let expected_process_subject = ctx
            .approved_subjects()
            .iter()
            .find(|subject| subject.kind == ToolSubjectKind::McpTrustClass)
            .cloned();
        let result = activate_lazy_mcp_tools_detailed_inner(
            &mut registry,
            &self.root_config,
            &self.provider_capabilities,
            self.workspace_root.clone(),
            Some(server_name),
            Arc::clone(&self.elicitation_handler),
            Arc::clone(&self.runtime_event_handler),
            ctx.mutation_recorder.clone(),
            expected_process_subject,
            ExtensionProcessNetworkAdmission::new(
                ctx.network_policy(),
                ctx.explicit_network_approval(),
            ),
        )
        .await?;
        Ok(activation_result(
            call_id,
            server_name,
            "ready",
            result.matched_servers,
            result.added_tools,
            &result.process_launch_receipts,
        ))
    }
}

impl McpActivateServerTool {
    fn lazy_server(&self, server_name: &str) -> Option<&McpServerConfig> {
        self.root_config.mcp_servers.iter().find(|server| {
            server.name == server_name
                && (server.startup == McpServerStartup::Lazy || server.streamable_http().is_some())
        })
    }

    fn registered_tool_count(&self, server_name: &str) -> usize {
        self.registry
            .upgrade()
            .map(|registry| {
                registry
                    .lifecycle_owners_by_scope(sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE, server_name)
                    .len()
            })
            .unwrap_or_default()
    }
}

fn mcp_server_process_network_effect(
    execution: &sigil_kernel::ExecutionConfig,
    workspace_root: &std::path::Path,
    server: &McpServerConfig,
    network_policy: NetworkPolicy,
) -> Option<NetworkEffect> {
    if network_policy != NetworkPolicy::Deny {
        return Some(NetworkEffect::Unknown);
    }
    let Some((command, args, inherit_env)) = server.stdio() else {
        return Some(NetworkEffect::Read);
    };
    let Ok(environment) = sigil_kernel::resolve_extension_process_environment(inherit_env) else {
        return Some(NetworkEffect::Unknown);
    };
    let Ok(plan) = sigil_tools_builtin::long_lived_stdio_process_plan(
        execution,
        command,
        args,
        workspace_root,
        &environment,
    ) else {
        return Some(NetworkEffect::Unknown);
    };
    if sigil_kernel::validate_extension_process_isolation_with_network_policy(
        plan.sandbox_profile,
        Some(NetworkEffect::Unknown),
        NetworkPolicy::Deny,
        plan.backend_capabilities,
        &plan.network,
        format!("mcp_server:{}", server.name),
    )
    .is_ok()
    {
        None
    } else {
        Some(NetworkEffect::Unknown)
    }
}

fn mcp_server_process_subjects(
    server: &McpServerConfig,
    workspace_root: &std::path::Path,
) -> Result<Vec<ToolSubject>> {
    let Some((_, _, inherit_env)) = server.stdio() else {
        return Ok(vec![
            mcp_server_subject(&server.name),
            ToolSubject::mcp_trust_class(server.name.clone(), server.trust.trust_class.as_str()),
        ]);
    };
    let environment = sigil_kernel::resolve_extension_process_environment(inherit_env)?;
    let (_, binding) = user_root_mcp_launch_binding(server, workspace_root)?;
    Ok(vec![
        mcp_server_subject(&server.name),
        ToolSubject::mcp_trust_class_with_process_binding(
            server.name.clone(),
            server.trust.trust_class.as_str(),
            binding.metadata.authorization_fingerprint,
            environment.live_fingerprint(),
        ),
    ])
}

struct UserRootMcpLaunchBinding {
    transport_static_fingerprint: String,
    metadata: McpDeclarationLaunchMetadata,
}

fn user_root_mcp_launch_binding(
    server: &McpServerConfig,
    workspace_root: &std::path::Path,
) -> Result<(ResolvedMcpStdioLaunch, UserRootMcpLaunchBinding)> {
    let declaration = ResolvedMcpServerDeclaration::user_root(server.clone(), workspace_root)?;
    let launch = declaration.resolve_stdio_launch(&[])?;
    let request = McpProcessLaunchRequest::from_config(server, Some(launch.cwd.clone()))?;
    let metadata = declaration.launch_metadata(
        &launch,
        &request.launch_static_fingerprint,
        request.environment.live_fingerprint(),
    );
    Ok((
        launch,
        UserRootMcpLaunchBinding {
            transport_static_fingerprint: request.launch_static_fingerprint,
            metadata,
        },
    ))
}

fn activation_result(
    call_id: String,
    server_name: &str,
    status: &str,
    matched_servers: usize,
    added_tools: usize,
    process_launch_receipts: &[McpProcessLaunchReceipt],
) -> ToolResult {
    ToolResult::ok(
        call_id,
        "mcp_activate_server",
        json!({
            "server_name": server_name,
            "status": status,
            "matched_servers": matched_servers,
            "added_tools": added_tools,
            "process_coverage": mcp_process_receipts_summary(process_launch_receipts),
        })
        .to_string(),
        ToolResultMeta::default(),
    )
}

#[must_use]
pub fn mcp_process_receipts_summary(receipts: &[McpProcessLaunchReceipt]) -> Option<String> {
    if receipts.is_empty() {
        return None;
    }
    Some(
        receipts
            .iter()
            .map(mcp_process_receipt_summary)
            .collect::<Vec<_>>()
            .join("; "),
    )
}

fn mcp_process_receipt_summary(receipt: &McpProcessLaunchReceipt) -> String {
    match receipt.coverage {
        McpProcessCoverage::LocalStdioSandboxed => {
            let backend = receipt
                .backend
                .map(|backend| backend.as_str())
                .unwrap_or("unknown");
            let profile = receipt
                .sandbox_profile
                .map(mcp_sandbox_profile_label)
                .unwrap_or("unknown");
            format!("sandboxed local stdio ({backend}, {profile})")
        }
        McpProcessCoverage::LocalStdioOutsideSandbox => {
            "local stdio outside local sandbox".to_owned()
        }
        McpProcessCoverage::RemoteOrExternal => {
            "external MCP; local sandbox does not apply".to_owned()
        }
        McpProcessCoverage::Unsupported => "MCP stdio sandbox unsupported".to_owned(),
    }
}

#[must_use]
pub fn mcp_stdio_boundary_summary(
    root_config: &RootConfig,
    workspace_root: &std::path::Path,
    server: &McpServerConfig,
) -> String {
    let Some((command, args, inherit_env)) = server.stdio() else {
        return "streamable HTTP; local stdio sandbox does not apply".to_owned();
    };
    let environment = match sigil_kernel::resolve_extension_process_environment(inherit_env) {
        Ok(environment) => environment,
        Err(error) => return format!("unsupported: {error}"),
    };
    match sigil_tools_builtin::long_lived_stdio_process_plan(
        &root_config.execution,
        command,
        args,
        workspace_root,
        &environment,
    ) {
        Ok(plan) if plan.sandboxed => {
            format!(
                "sandboxed local stdio ({}, {}, network={})",
                plan.backend.as_str(),
                mcp_sandbox_profile_label(plan.sandbox_profile),
                plan.network.policy.as_str(),
            )
        }
        Ok(plan) => format!(
            "local stdio outside local sandbox (network={})",
            plan.network.policy.as_str()
        ),
        Err(error) => format!("unsupported: {error}"),
    }
}

fn mcp_sandbox_profile_label(profile: sigil_kernel::ExecutionSandboxProfile) -> &'static str {
    match profile {
        sigil_kernel::ExecutionSandboxProfile::Unconfined => "unconfined",
        sigil_kernel::ExecutionSandboxProfile::WorkspaceWrite => "workspace_write",
        sigil_kernel::ExecutionSandboxProfile::BuildOffline => "build_offline",
        sigil_kernel::ExecutionSandboxProfile::BuildNetworked => "build_networked",
    }
}

fn required_server_name(args: &Value) -> Result<&str> {
    args.get("server_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("missing server_name"))
}

fn mcp_server_subject(server_name: &str) -> ToolSubject {
    ToolSubject {
        kind: ToolSubjectKind::McpTool,
        original: server_name.to_owned(),
        normalized: format!("mcp_server:{server_name}"),
        canonical_path: None,
        scope: ToolSubjectScope::Unknown,
    }
}

#[cfg(test)]
#[path = "tests/mcp_registry_tests.rs"]
mod tests;
