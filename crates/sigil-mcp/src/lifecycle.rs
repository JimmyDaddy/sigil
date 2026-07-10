use super::*;

pub const MCP_TOOL_LIFECYCLE_NAMESPACE: &str = "sigil.mcp.server";

pub struct McpToolRegistrationOptions {
    pub provider_tool_name_max_chars: usize,
    pub roots: Vec<PathBuf>,
    pub working_dir: Option<PathBuf>,
    pub secret_redactor: SecretRedactor,
    pub elicitation_handler: Arc<dyn McpElicitationHandler>,
    pub runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    pub startup: McpServerStartup,
    pub mutation_recorder: Option<MutationEventRecorder>,
    pub mutation_workspace_root: Option<PathBuf>,
    pub process_launcher: Arc<dyn McpProcessLauncher>,
    pub expected_process_subject: Option<ToolSubject>,
    pub strict_registration: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpToolRegistrationReport {
    pub process_launch_receipts: Vec<McpProcessLaunchReceipt>,
    pub lifecycle_owners: Vec<ToolLifecycleOwner>,
}

impl McpToolRegistrationOptions {
    pub fn eager() -> Result<Self> {
        Self::for_startup(McpServerStartup::Eager)
    }

    pub fn lazy() -> Result<Self> {
        Self::for_startup(McpServerStartup::Lazy)
    }

    pub fn for_startup(startup: McpServerStartup) -> Result<Self> {
        Ok(Self {
            provider_tool_name_max_chars: DEFAULT_PROVIDER_TOOL_NAME_MAX_CHARS,
            roots: default_mcp_roots()?,
            working_dir: None,
            secret_redactor: SecretRedactor::empty(),
            elicitation_handler: unsupported_mcp_elicitation_handler(),
            runtime_event_handler: unsupported_mcp_runtime_event_handler(),
            startup,
            mutation_recorder: None,
            mutation_workspace_root: None,
            process_launcher: Arc::new(LocalMcpProcessLauncher),
            expected_process_subject: None,
            strict_registration: false,
        })
    }

    pub fn with_capabilities(mut self, capabilities: &ProviderCapabilities) -> Self {
        self.provider_tool_name_max_chars = capabilities.tool_name_max_chars;
        self
    }

    pub fn with_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.roots = roots;
        self
    }

    pub fn with_working_dir(mut self, working_dir: PathBuf) -> Self {
        self.working_dir = Some(working_dir);
        self
    }

    pub fn with_secret_redactor(mut self, secret_redactor: SecretRedactor) -> Self {
        self.secret_redactor = secret_redactor;
        self
    }

    pub fn with_elicitation_handler(
        mut self,
        elicitation_handler: Arc<dyn McpElicitationHandler>,
    ) -> Self {
        self.elicitation_handler = elicitation_handler;
        self
    }

    pub fn with_runtime_event_handler(
        mut self,
        runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
    ) -> Self {
        self.runtime_event_handler = runtime_event_handler;
        self
    }

    pub fn with_mutation_recorder(
        mut self,
        workspace_root: PathBuf,
        mutation_recorder: MutationEventRecorder,
    ) -> Self {
        self.mutation_workspace_root = Some(workspace_root);
        self.mutation_recorder = Some(mutation_recorder);
        self
    }

    pub fn with_process_launcher(mut self, process_launcher: Arc<dyn McpProcessLauncher>) -> Self {
        self.process_launcher = process_launcher;
        self
    }

    /// Requires the launch-time process binding to match the exact subject approved by the agent.
    #[must_use]
    pub fn with_expected_process_subject(mut self, subject: ToolSubject) -> Self {
        self.expected_process_subject = Some(subject);
        self
    }

    /// Requires each selected server to produce a live callable replacement generation.
    #[must_use]
    pub fn with_strict_registration(mut self) -> Self {
        self.strict_registration = true;
        self
    }
}

pub async fn register_mcp_tools(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
) -> Result<()> {
    register_mcp_tools_with_options(registry, servers, McpToolRegistrationOptions::eager()?).await
}

pub async fn activate_lazy_mcp_tools(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
) -> Result<()> {
    register_mcp_tools_with_options(registry, servers, McpToolRegistrationOptions::lazy()?).await
}

pub async fn register_mcp_tools_with_options(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<()> {
    register_mcp_tools_with_report(registry, servers, options)
        .await
        .map(|_| ())
}

pub async fn register_mcp_tools_with_report(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<McpToolRegistrationReport> {
    register_mcp_tools_for_startup(registry, servers, options).await
}

pub(super) async fn register_mcp_tools_for_startup(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
) -> Result<McpToolRegistrationReport> {
    let mut registered_owners = Vec::new();
    let result =
        register_mcp_tools_for_startup_inner(registry, servers, options, &mut registered_owners)
            .await;
    match result {
        Ok(report) => Ok(report),
        Err(error) => match rollback_registered_mcp_generations(registry, &registered_owners).await
        {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(error.context(format!(
                "MCP registration rollback was incomplete: {rollback_error:#}"
            ))),
        },
    }
}

async fn register_mcp_tools_for_startup_inner(
    registry: &mut ToolRegistry,
    servers: &[McpServerConfig],
    options: McpToolRegistrationOptions,
    registered_owners: &mut Vec<ToolLifecycleOwner>,
) -> Result<McpToolRegistrationReport> {
    let mut unique_server_names = BTreeSet::new();
    for server in servers {
        if !unique_server_names.insert(server.name.as_str()) {
            bail!("duplicate MCP server name is not allowed: {}", server.name);
        }
    }
    let mut used_provider_names = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    let mut report = McpToolRegistrationReport::default();
    for server in servers {
        if server.startup != options.startup {
            if options.startup == McpServerStartup::Eager
                && server.startup == McpServerStartup::Lazy
            {
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    "lazy MCP server startup is deferred until explicit activation"
                );
            }
            continue;
        }

        let lifecycle_scan = capture_mcp_server_lifecycle_scan(&options, &server.name)?;
        let client = match McpClient::spawn(
            server.clone(),
            options.roots.clone(),
            options.working_dir.clone(),
            options.secret_redactor.clone(),
            Arc::clone(&options.elicitation_handler),
            Arc::clone(&options.runtime_event_handler),
            Arc::clone(&options.process_launcher),
            options.expected_process_subject.as_ref(),
        )
        .await
        {
            Ok(client) => client,
            Err(error) if !server.required && !options.strict_registration => {
                let receipt = error
                    .downcast_ref::<super::client::McpPostSpawnStartupError>()
                    .map(super::client::McpPostSpawnStartupError::receipt);
                let cleanup_incomplete = error
                    .downcast_ref::<super::client::McpPostSpawnStartupError>()
                    .is_some_and(|error| !error.cleanup_completed());
                record_mcp_server_lifecycle_scan_result(
                    &options,
                    &server.name,
                    lifecycle_scan.as_ref(),
                    receipt,
                    "startup_failed",
                )?;
                if cleanup_incomplete {
                    return Err(error.context(format!(
                        "optional MCP server {} startup cleanup was incomplete",
                        server.name
                    )));
                }
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    error = %error,
                    "optional MCP server failed to start and will be skipped"
                );
                continue;
            }
            Err(error) => {
                let receipt = error
                    .downcast_ref::<super::client::McpPostSpawnStartupError>()
                    .map(super::client::McpPostSpawnStartupError::receipt);
                record_mcp_server_lifecycle_scan_result(
                    &options,
                    &server.name,
                    lifecycle_scan.as_ref(),
                    receipt,
                    "startup_failed",
                )?;
                return Err(error);
            }
        };
        let process_receipt = client.process_receipt().clone();
        let tools = match client.list_tools().await {
            Ok(tools) => tools,
            Err(error) if !server.required && !options.strict_registration => {
                let cleanup = client
                    .close_connection(format!("tools/list failed: {error:#}"))
                    .await;
                record_mcp_server_lifecycle_scan_result(
                    &options,
                    &server.name,
                    lifecycle_scan.as_ref(),
                    Some(&process_receipt),
                    "tools_list_failed",
                )?;
                if !cleanup.completed {
                    bail!(
                        "optional MCP server {} tools/list failed and cleanup was incomplete: {error:#}; transport cleanup: {}",
                        server.name,
                        cleanup.summary()
                    );
                }
                warn!(
                    server = %server.name,
                    trust_class = server.trust.trust_class.as_str(),
                    error = %error,
                    cleanup = %cleanup.summary(),
                    "optional MCP server tools/list failed and will be skipped"
                );
                continue;
            }
            Err(error) => {
                let cleanup = client
                    .close_connection(format!("tools/list failed: {error:#}"))
                    .await;
                record_mcp_server_lifecycle_scan_result(
                    &options,
                    &server.name,
                    lifecycle_scan.as_ref(),
                    Some(&process_receipt),
                    "tools_list_failed",
                )?;
                bail!(
                    "MCP server {} tools/list failed: {error:#}; transport cleanup: {}",
                    server.name,
                    cleanup.summary()
                );
            }
        };
        let mut registered_surface_count = 0usize;
        for tool in tools {
            let tool_name = McpToolName::new(
                &server.name,
                &tool.name,
                options.provider_tool_name_max_chars,
                &mut used_provider_names,
            );
            registry.register(Arc::new(McpTool {
                client: Arc::clone(&client),
                spec: ToolSpec {
                    name: tool_name.provider_name.clone(),
                    description: tool.description.unwrap_or_else(|| "MCP tool".to_owned()),
                    input_schema: tool.input_schema,
                    category: ToolCategory::Mcp,
                    access: ToolAccess::Network,
                    preview: ToolPreviewCapability::None,
                },
                tool_name,
                trust: server.trust.clone(),
            }));
            registered_surface_count = registered_surface_count.saturating_add(1);
        }
        if client.supports_resources() {
            for resource_kind in McpResourceToolKind::all() {
                let original_name = resource_kind.provider_suffix();
                let tool_name = McpToolName::new(
                    &server.name,
                    original_name,
                    options.provider_tool_name_max_chars,
                    &mut used_provider_names,
                );
                registry.register(Arc::new(McpResourceTool {
                    client: Arc::clone(&client),
                    spec: ToolSpec {
                        name: tool_name.provider_name.clone(),
                        description: resource_kind.description().to_owned(),
                        input_schema: resource_kind.input_schema(),
                        category: ToolCategory::Mcp,
                        access: ToolAccess::Read,
                        preview: ToolPreviewCapability::None,
                    },
                    tool_name,
                    kind: resource_kind,
                    trust: server.trust.clone(),
                }));
                registered_surface_count = registered_surface_count.saturating_add(1);
            }
        }
        if client.supports_prompts() {
            for prompt_kind in McpPromptToolKind::all() {
                let original_name = prompt_kind.provider_suffix();
                let tool_name = McpToolName::new(
                    &server.name,
                    original_name,
                    options.provider_tool_name_max_chars,
                    &mut used_provider_names,
                );
                registry.register(Arc::new(McpPromptTool {
                    client: Arc::clone(&client),
                    spec: ToolSpec {
                        name: tool_name.provider_name.clone(),
                        description: prompt_kind.description().to_owned(),
                        input_schema: prompt_kind.input_schema(),
                        category: ToolCategory::Mcp,
                        access: ToolAccess::Read,
                        preview: ToolPreviewCapability::None,
                    },
                    tool_name,
                    kind: prompt_kind,
                    trust: server.trust.clone(),
                }));
                registered_surface_count = registered_surface_count.saturating_add(1);
            }
        }
        if registered_surface_count == 0 {
            let cleanup = client
                .close_connection("MCP server registered no callable surfaces".to_owned())
                .await;
            if let Err(error) = record_mcp_server_lifecycle_scan_result(
                &options,
                &server.name,
                lifecycle_scan.as_ref(),
                Some(&process_receipt),
                "zero_surface",
            ) {
                return Err(error.context(format!(
                    "MCP server {} zero-surface lifecycle evidence failed; transport cleanup: {}",
                    server.name,
                    cleanup.summary()
                )));
            }
            if !cleanup.completed || options.strict_registration || server.required {
                bail!(
                    "MCP server {} registered no callable surfaces{}: {}",
                    server.name,
                    if !cleanup.completed {
                        " and cleanup was incomplete"
                    } else if options.strict_registration {
                        " during strict replacement"
                    } else {
                        " although the server is required"
                    },
                    cleanup.summary()
                );
            }
            warn!(
                server = %server.name,
                cleanup = %cleanup.summary(),
                "optional MCP server registered no callable surfaces and will be skipped"
            );
        } else {
            let owner = client.lifecycle_owner();
            registered_owners.push(owner.clone());
            if let Err(error) = record_mcp_server_lifecycle_scan_result(
                &options,
                &server.name,
                lifecycle_scan.as_ref(),
                Some(&process_receipt),
                "registered",
            ) {
                let cleanup = client
                    .close_connection(format!("registered lifecycle evidence failed: {error:#}"))
                    .await;
                return Err(error.context(format!(
                    "MCP server {} lifecycle evidence failed after spawn and callable owner registration; transport cleanup: {}",
                    server.name,
                    cleanup.summary()
                )));
            }
            report.process_launch_receipts.push(process_receipt);
            report.lifecycle_owners.push(owner);
        }
    }
    Ok(report)
}

async fn rollback_registered_mcp_generations(
    registry: &mut ToolRegistry,
    owners: &[ToolLifecycleOwner],
) -> Result<()> {
    let mut failures = Vec::new();
    for owner in owners.iter().rev() {
        let tools = registry.drain_by_lifecycle_owner(owner);
        let Some(tool) = tools.first() else {
            continue;
        };
        if let Err(error) = tool.shutdown().await {
            failures.push(format!(
                "failed to shut down MCP generation {} for scope {}: {error:#}",
                owner.generation(),
                owner.scope()
            ));
        }
    }
    if !failures.is_empty() {
        bail!(failures.join("; "));
    }
    Ok(())
}

pub(super) fn capture_mcp_server_lifecycle_scan(
    options: &McpToolRegistrationOptions,
    server_name: &str,
) -> Result<Option<WorkspaceMutationScan>> {
    let (Some(recorder), Some(workspace_root)) =
        (&options.mutation_recorder, &options.mutation_workspace_root)
    else {
        return Ok(None);
    };
    let scope = VerificationScope::all_tracked(DEFAULT_TASK_VERIFICATION_SCOPE_HASH);
    match recorder.capture_workspace_scan(workspace_root, &scope) {
        Ok(scan) => Ok(Some(scan)),
        Err(error) => {
            let mut metadata = mcp_lifecycle_metadata(None, "scan_unavailable_before_startup");
            metadata.insert(
                "mcp_lifecycle_scan".to_owned(),
                "before_unavailable".to_owned(),
            );
            recorder
                .record_external_process_unknown_dirty_with_metadata(
                    workspace_root,
                    format!("mcp_server:{server_name}"),
                    ToolEffect::Unknown,
                    metadata,
                )
                .with_context(|| {
                    format!("failed to record MCP server {server_name} lifecycle mutation evidence")
                })?;
            warn!(
                server = %server_name,
                error = %error,
                "failed to capture MCP server lifecycle workspace scan before startup"
            );
            Ok(None)
        }
    }
}

pub(super) fn record_mcp_server_lifecycle_scan_result(
    options: &McpToolRegistrationOptions,
    server_name: &str,
    before: Option<&WorkspaceMutationScan>,
    receipt: Option<&McpProcessLaunchReceipt>,
    startup_result: &'static str,
) -> Result<()> {
    let (Some(recorder), Some(workspace_root)) =
        (&options.mutation_recorder, &options.mutation_workspace_root)
    else {
        return Ok(());
    };
    let metadata = mcp_lifecycle_metadata(receipt, startup_result);
    let status = match startup_result {
        "registered" => ExtensionProcessLifecycleStatus::Registered,
        "startup_failed" => ExtensionProcessLifecycleStatus::StartupFailed,
        "zero_surface" => ExtensionProcessLifecycleStatus::StartupFailed,
        "tools_list_failed" => ExtensionProcessLifecycleStatus::ToolsListFailed,
        _ => {
            bail!("unsupported MCP lifecycle result {startup_result}");
        }
    };
    recorder
        .append_extension_process_lifecycle(&ExtensionProcessLifecycleAudit {
            process_kind: "mcp_stdio".to_owned(),
            subject: server_name.to_owned(),
            phase: if receipt.is_some() {
                ExtensionProcessLaunchPhase::PostSpawn
            } else {
                ExtensionProcessLaunchPhase::PreSpawn
            },
            status,
            safe_metadata: metadata.clone(),
        })
        .with_context(|| {
            format!("failed to record MCP server {server_name} durable lifecycle audit")
        })?;
    let process_name = format!("mcp_server:{server_name}");
    let Some(before) = before else {
        recorder
            .record_external_process_unknown_dirty_with_metadata(
                workspace_root,
                process_name,
                ToolEffect::Unknown,
                metadata,
            )
            .with_context(|| {
                format!(
                    "failed to record MCP server {server_name} lifecycle receipt after the pre-start scan was unavailable"
                )
            })?;
        return Ok(());
    };
    match recorder.capture_workspace_scan(workspace_root, &before.scope) {
        Ok(after) => {
            recorder.record_external_process_mutation_scan_result(
                before,
                &after,
                process_name,
                ToolEffect::Unknown,
                metadata,
            )?;
        }
        Err(error) => {
            recorder
                .record_external_process_scan_unavailable_after(
                    before,
                    process_name,
                    ToolEffect::Unknown,
                    metadata,
                )
                .with_context(|| {
                    format!("failed to record MCP server {server_name} lifecycle mutation evidence")
                })?;
            warn!(
                server = %server_name,
                error = %error,
                "failed to capture MCP server lifecycle workspace scan after startup"
            );
        }
    }
    Ok(())
}

pub(super) fn mcp_lifecycle_metadata(
    receipt: Option<&McpProcessLaunchReceipt>,
    startup_result: &'static str,
) -> BTreeMap<String, String> {
    let mut metadata = receipt
        .map(McpProcessLaunchReceipt::audit_metadata)
        .unwrap_or_else(|| {
            BTreeMap::from([(
                "mcp_process_coverage".to_owned(),
                McpProcessCoverage::Unsupported.as_str().to_owned(),
            )])
        });
    metadata.insert("mcp_startup_result".to_owned(), startup_result.to_owned());
    metadata
}

pub(super) fn default_mcp_roots() -> Result<Vec<PathBuf>> {
    let cwd =
        std::env::current_dir().context("failed to resolve current directory for MCP roots")?;
    Ok(vec![canonical_root(cwd)])
}
