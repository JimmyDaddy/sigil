use super::*;

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
    let mut registry = ToolRegistry::new();
    register_local_tools(&mut registry, root_config, workspace_root.clone())?;
    sigil_mcp::register_mcp_tools_with_options(
        &mut registry,
        &root_config.mcp_servers,
        sigil_mcp::McpToolRegistrationOptions::eager()?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
            .with_working_dir(workspace_root.clone())
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(Arc::clone(&elicitation_handler))
            .with_runtime_event_handler(Arc::clone(&runtime_event_handler))
            .with_process_launcher(configured_mcp_process_launcher(root_config)),
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
    Ok(registry)
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
    let mut registry = ToolRegistry::new();
    register_local_tools(&mut registry, root_config, workspace_root.clone())?;
    register_lazy_mcp_activation_tool(
        &mut registry,
        root_config,
        provider_capabilities,
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    );
    Ok(registry)
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
    let servers = root_config
        .mcp_servers
        .iter()
        .filter(|server| server.startup == McpServerStartup::Lazy)
        .filter(|server| server_name.is_none_or(|name| server.name == name))
        .cloned()
        .collect::<Vec<_>>();
    if servers.is_empty() {
        return Ok(LazyMcpActivationResult {
            matched_servers: 0,
            added_tools: 0,
            process_launch_receipts: Vec::new(),
        });
    }

    let before = registry.specs().len();
    let mut registration_options = sigil_mcp::McpToolRegistrationOptions::lazy()?
        .with_capabilities(provider_capabilities)
        .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
        .with_working_dir(workspace_root.clone())
        .with_secret_redactor(secret_redactor_for_root_config(root_config))
        .with_elicitation_handler(elicitation_handler)
        .with_runtime_event_handler(runtime_event_handler)
        .with_process_launcher(configured_mcp_process_launcher(root_config));
    if let Some(recorder) = mutation_recorder {
        registration_options =
            registration_options.with_mutation_recorder(workspace_root.clone(), recorder);
    }
    let report =
        sigil_mcp::register_mcp_tools_with_report(registry, &servers, registration_options).await?;
    Ok(LazyMcpActivationResult {
        matched_servers: servers.len(),
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
) -> Result<()> {
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
    sigil_code_intel::register_code_intelligence_tools(
        registry,
        &root_config.code_intelligence,
        workspace_root.clone(),
    );
    let user_config_dir = default_user_config_dir().ok();
    let _ = skills::register_skill_tools_with_project_assets_root(
        registry,
        &workspace_root,
        &paths.project_assets_root,
        user_config_dir.as_deref(),
        &root_config.skills,
    );
    Ok(())
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

fn configured_mcp_process_launcher(root_config: &RootConfig) -> Arc<dyn McpProcessLauncher> {
    Arc::new(ConfiguredMcpProcessLauncher {
        execution: root_config.execution.clone(),
    })
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
            &request.env,
        )?;
        let mut command = Command::new(&plan.program);
        command
            .args(&plan.args)
            .current_dir(&plan.cwd)
            .envs(&plan.env)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
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
            },
        })
    }
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
    let servers = root_config
        .mcp_servers
        .iter()
        .filter(|server| server.name == server_name)
        .cloned()
        .collect::<Vec<_>>();
    let Some(server) = servers.first() else {
        return Ok(McpRefreshResult {
            matched_servers: 0,
            removed_tools: 0,
            added_tools: 0,
            process_launch_receipts: Vec::new(),
        });
    };

    let prefix = sigil_mcp::mcp_provider_tool_name_prefix(server_name);
    let removed = registry.drain_by_name_prefix(&prefix);
    let removed_tools = removed.len();
    let before = registry.specs().len();
    let mut registration_options =
        sigil_mcp::McpToolRegistrationOptions::for_startup(server.startup)?
            .with_capabilities(provider_capabilities)
            .with_roots(vec![canonical_workspace_root(workspace_root.clone())])
            .with_working_dir(workspace_root.clone())
            .with_secret_redactor(secret_redactor_for_root_config(root_config))
            .with_elicitation_handler(elicitation_handler)
            .with_runtime_event_handler(runtime_event_handler)
            .with_process_launcher(configured_mcp_process_launcher(root_config));
    if let Some(recorder) = mutation_recorder {
        registration_options =
            registration_options.with_mutation_recorder(workspace_root.clone(), recorder);
    }
    let report =
        match sigil_mcp::register_mcp_tools_with_report(registry, &servers, registration_options)
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
    Ok(McpRefreshResult {
        matched_servers: servers.len(),
        removed_tools,
        added_tools: registry.specs().len().saturating_sub(before),
        process_launch_receipts: report.process_launch_receipts,
    })
}

pub(super) fn register_lazy_mcp_activation_tool(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_capabilities: &ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
) {
    if !root_config
        .mcp_servers
        .iter()
        .any(|server| server.startup == McpServerStartup::Lazy)
    {
        return;
    }
    registry.register(Arc::new(McpActivateServerTool {
        registry: registry.clone(),
        root_config: root_config.clone(),
        provider_capabilities: provider_capabilities.clone(),
        workspace_root,
        elicitation_handler,
        runtime_event_handler,
    }));
}

#[derive(Clone)]
struct McpActivateServerTool {
    registry: ToolRegistry,
    root_config: RootConfig,
    provider_capabilities: ProviderCapabilities,
    workspace_root: PathBuf,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
    runtime_event_handler: Arc<dyn McpRuntimeEventHandler>,
}

#[async_trait]
impl Tool for McpActivateServerTool {
    fn spec(&self) -> ToolSpec {
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
            access: ToolAccess::Network,
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_subjects(&self, _ctx: &ToolContext, args: &Value) -> Result<Vec<ToolSubject>> {
        let server_name = required_server_name(args)?;
        let Some(server) = self.lazy_server(server_name) else {
            return Ok(vec![mcp_server_subject(server_name)]);
        };
        Ok(vec![
            mcp_server_subject(server_name),
            ToolSubject::mcp_trust_class(server.name.clone(), server.trust.trust_class.as_str()),
        ])
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
        Ok(Some(ToolEgressAudit {
            destination: format!("mcp:{server_name}"),
            operation: "server/activate".to_owned(),
            payload: json!({
                "server": server_name,
                "trust_class": server.trust.trust_class.as_str(),
                "startup": server.startup.as_str(),
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

        let mut registry = self.registry.clone();
        let result = activate_lazy_mcp_tools_detailed_with_mcp_handlers_and_mutation_recorder(
            &mut registry,
            &self.root_config,
            &self.provider_capabilities,
            self.workspace_root.clone(),
            Some(server_name),
            Arc::clone(&self.elicitation_handler),
            Arc::clone(&self.runtime_event_handler),
            ctx.mutation_recorder.clone(),
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
        self.root_config
            .mcp_servers
            .iter()
            .find(|server| server.name == server_name && server.startup == McpServerStartup::Lazy)
    }

    fn registered_tool_count(&self, server_name: &str) -> usize {
        let prefix = sigil_mcp::mcp_provider_tool_name_prefix(server_name);
        self.registry
            .specs()
            .into_iter()
            .filter(|spec| spec.name.starts_with(&prefix))
            .count()
    }
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
    match sigil_tools_builtin::long_lived_stdio_process_plan(
        &root_config.execution,
        &server.command,
        &server.args,
        workspace_root,
        &Default::default(),
    ) {
        Ok(plan) if plan.sandboxed => {
            format!(
                "sandboxed local stdio ({}, {})",
                plan.backend.as_str(),
                mcp_sandbox_profile_label(plan.sandbox_profile)
            )
        }
        Ok(_) => "local stdio outside local sandbox".to_owned(),
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
