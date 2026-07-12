use std::{collections::BTreeSet, net::IpAddr, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, EgressBindingOrigin, EgressDataCategory, EgressDisclosurePresenter,
    EgressNetworkRoute, McpRemoteClientCapability, McpServerConfig, NetworkEffect, NetworkPolicy,
    RootConfig, SecretString, Tool, ToolAccess, ToolCategory, ToolContext, ToolEgressAudit,
    ToolErrorKind, ToolLifecycleOwner, ToolOperation, ToolPreviewCapability, ToolRegistry,
    ToolResult, ToolResultMeta, ToolSpec, ToolSubject, WebTaskTreeBudgetLimits,
    safe_persistence_text,
};
use sigil_mcp::{
    McpElicitationAction, McpElicitationHandler, McpElicitationRequest,
    McpRemoteClientCapabilities, McpRemoteFormHandler, McpRemoteFormResponse, McpRemoteRoot,
    McpRemoteTool, McpStreamableHttpClient, McpStreamableHttpHeaderConfig,
    McpStreamableHttpHeaderEnvironment, McpStreamableHttpLimits, McpToolName,
    PreparedMcpStreamableHttpHeaders, ValidatedMcpFormRequest,
};
use url::Url;

use crate::{
    EgressOrderingCoordinator, IpCidr, ProxyEnvironment,
    RuntimeMcpStreamableHttpDestinationAuthorizer, RuntimeMcpTransportAttemptFactory,
    SystemWebDestinationResolver, WebDestinationGuard, WebDestinationGuardPolicy,
    secret_redactor_for_root_config,
};

struct ProcessHeaderEnvironment;

impl McpStreamableHttpHeaderEnvironment for ProcessHeaderEnvironment {
    fn resolve(&self, name: &str) -> Option<SecretString> {
        std::env::var(name).ok().map(SecretString::new)
    }
}

/// Activates one configured eager Streamable HTTP server outside an agent tool call.
///
/// Eager startup is admitted only for an effective `allow` network policy; `ask` remains
/// fail-closed because no explicit user approval interaction exists at background startup time.
#[allow(clippy::too_many_arguments)]
pub async fn activate_eager_remote_mcp_server(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    server_name: &str,
    provider_tool_name_max_chars: usize,
    workspace_root: std::path::PathBuf,
    recorder: sigil_kernel::EgressAuditRecorder,
    presenter: Arc<dyn EgressDisclosurePresenter>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<usize> {
    let server = root_config
        .mcp_servers
        .iter()
        .find(|server| server.name == server_name)
        .ok_or_else(|| anyhow!("unknown eager remote MCP server {server_name}"))?;
    if server.streamable_http().is_none() {
        bail!("eager remote MCP activation requires streamable_http transport");
    }
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        format!("remote-mcp-eager-run-{}", uuid::Uuid::new_v4()),
        web_budget_limits(root_config),
        None,
    )?;
    let context = ToolContext::for_eager_network_startup(
        workspace_root,
        root_config.web.timeout_secs,
        root_config.web.network_mode,
        recorder,
        budget,
    )?;
    activate_remote_mcp_server(
        registry,
        root_config,
        server,
        provider_tool_name_max_chars,
        &context,
        presenter,
        elicitation_handler,
    )
    .await
}

/// Activates one user-root Streamable HTTP MCP server after the ordinary tool permission decision
/// has admitted network egress. Every HTTP message still passes the durable authorization,
/// presentation, destination-guard, and shared-budget barrier before DNS or socket activity.
pub async fn activate_remote_mcp_server(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    server: &McpServerConfig,
    provider_tool_name_max_chars: usize,
    context: &ToolContext,
    presenter: Arc<dyn EgressDisclosurePresenter>,
    elicitation_handler: Arc<dyn McpElicitationHandler>,
) -> Result<usize> {
    let remote = server
        .streamable_http()
        .ok_or_else(|| anyhow!("remote MCP activation requires streamable_http transport"))?;
    if context.network_policy() == NetworkPolicy::Deny
        || (context.network_policy() == NetworkPolicy::Ask && !context.explicit_network_approval())
    {
        bail!("remote MCP activation requires current network authorization");
    }
    let recorder = context
        .egress_audit_recorder()
        .ok_or_else(|| anyhow!("remote MCP activation requires a durable session recorder"))?;
    let endpoint = Url::parse(&remote.url).context("invalid remote MCP endpoint")?;
    enforce_allowed_domain(root_config, &endpoint)?;
    let endpoint_secret = SecretString::new(remote.url.clone());
    let header_config = McpStreamableHttpHeaderConfig {
        literal: remote.http_headers.clone(),
        from_env: remote.env_http_headers.clone(),
        bearer_token_env_var: remote.bearer_token_env_var.clone(),
    };
    let prepared = PreparedMcpStreamableHttpHeaders::prepare(
        endpoint_secret.clone(),
        &header_config,
        &ProcessHeaderEnvironment,
    )?;
    let live_header_fingerprint = prepared.live_header_fingerprint().to_owned();
    let transport_fingerprint = sigil_mcp::mcp_transport_static_fingerprint(server)?;
    let proxy = proxy_environment(root_config);
    let policy = destination_policy(root_config)?;
    let guard = Arc::new(WebDestinationGuard::new(
        SystemWebDestinationResolver,
        policy,
        proxy,
    ));
    let preview = guard.preview(endpoint)?;
    let route = if preview.is_proxy_remote() {
        EgressNetworkRoute::ProxyRemote
    } else {
        EgressNetworkRoute::Direct
    };
    let profile_config_proxy_fingerprint = sha256_fingerprint(&format!(
        "{}\0{}\0{}\0{}",
        transport_fingerprint,
        live_header_fingerprint,
        root_config.web.proxy_mode as u8,
        preview.safe_transport_destination()
    ));
    let budget = context
        .web_task_tree_budget()
        .ok_or_else(|| anyhow!("remote MCP activation requires a root-owned task-tree budget"))?;
    let root_run_id = budget.root_run_id().to_owned();
    let attempts = Arc::new(RuntimeMcpTransportAttemptFactory::new(
        budget,
        root_run_id,
        EgressBindingOrigin::UserConfigured,
        format!("remote-mcp-{}-transport-v1", server.name),
        "mcp",
        format!("MCP server {}", server.name),
        transport_fingerprint.clone(),
        profile_config_proxy_fingerprint.clone(),
        preview.safe_logical_destination(),
        preview.safe_transport_destination(),
        route,
        vec![EgressDataCategory::ConnectionMetadata],
    ));
    let cancellation = context.cancellation_handle();
    let admission_is_live: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        cancellation
            .as_ref()
            .is_none_or(|cancellation| !cancellation.is_cancel_requested())
    });
    let authorizer_attempts: Arc<dyn crate::RuntimeMcpStreamableHttpAttemptFactory> =
        attempts.clone();
    let authorizer = Arc::new(
        RuntimeMcpStreamableHttpDestinationAuthorizer::new(
            endpoint_secret,
            guard,
            EgressOrderingCoordinator::new(recorder, Some(presenter)),
            authorizer_attempts,
            profile_config_proxy_fingerprint,
            live_header_fingerprint,
            admission_is_live,
        )
        .with_transport_fingerprint(transport_fingerprint.clone()),
    );
    let capabilities = McpRemoteClientCapabilities {
        roots: remote
            .client_capabilities
            .contains(&McpRemoteClientCapability::Roots),
        form_elicitation: remote
            .client_capabilities
            .contains(&McpRemoteClientCapability::ElicitationForm),
    };
    let roots = if capabilities.roots {
        vec![remote_workspace_root(&context.workspace_root)?]
    } else {
        Vec::new()
    };
    let form_handler = if capabilities.form_elicitation {
        if !elicitation_handler.supports_elicitation() {
            bail!("remote MCP form elicitation requires a concrete product-surface handler");
        }
        Some(Arc::new(RemoteFormHandlerAdapter {
            server_name: server.name.clone(),
            handler: elicitation_handler,
        }) as Arc<dyn McpRemoteFormHandler>)
    } else {
        None
    };
    let client = McpStreamableHttpClient::connect_with_inbound_prepared(
        authorizer,
        prepared,
        capabilities,
        McpStreamableHttpLimits {
            max_header_bytes: 32 * 1024,
            max_body_bytes: root_config.web.max_wire_response_bytes as usize,
            max_sse_line_bytes: 64 * 1024,
            max_sse_event_bytes: root_config.web.max_decoded_response_bytes as usize,
            max_sse_events: 256,
            response_timeout: Duration::from_secs(root_config.web.timeout_secs),
        },
        roots,
        form_handler,
    )
    .await?;
    validate_remote_pin(server, &transport_fingerprint, &client).await?;
    let tools = client.list_tools().await?;
    let generation = format!("remote-mcp-generation-{}", uuid::Uuid::new_v4());
    let lifecycle_owner = ToolLifecycleOwner::new(
        sigil_mcp::MCP_TOOL_LIFECYCLE_NAMESPACE,
        server.name.clone(),
        generation,
    );
    let redactor = secret_redactor_for_root_config(root_config);
    let execution_lock = Arc::new(tokio::sync::Mutex::new(()));
    let mut used = registry
        .specs()
        .into_iter()
        .map(|spec| spec.name)
        .collect::<BTreeSet<_>>();
    let before = used.len();
    for remote_tool in tools {
        let name = McpToolName::new(
            &server.name,
            &remote_tool.name,
            provider_tool_name_max_chars,
            &mut used,
        );
        registry.register(Arc::new(RemoteMcpTool {
            client: Arc::clone(&client),
            remote_tool,
            provider_name: name.provider_name,
            server_name: server.name.clone(),
            trust: server.trust.clone(),
            lifecycle_owner: lifecycle_owner.clone(),
            redactor: redactor.clone(),
            attempts: Arc::clone(&attempts),
            execution_lock: Arc::clone(&execution_lock),
        }));
    }
    Ok(used.len().saturating_sub(before))
}

struct RemoteFormHandlerAdapter {
    server_name: String,
    handler: Arc<dyn McpElicitationHandler>,
}

#[async_trait]
impl McpRemoteFormHandler for RemoteFormHandlerAdapter {
    async fn handle_form(
        &self,
        request: ValidatedMcpFormRequest,
    ) -> Result<McpRemoteFormResponse, sigil_mcp::McpStreamableHttpError> {
        let requested_schema = request.requested_schema().clone();
        let response = self
            .handler
            .elicit(McpElicitationRequest {
                server_name: self.server_name.clone(),
                message: request.safe_message,
                requested_schema,
            })
            .await
            .map_err(|_| sigil_mcp::McpStreamableHttpError::Transport)?;
        Ok(match response.action {
            McpElicitationAction::Accept => {
                McpRemoteFormResponse::Accept(response.content.unwrap_or_else(|| json!({})))
            }
            McpElicitationAction::Decline => McpRemoteFormResponse::Decline,
            McpElicitationAction::Cancel => McpRemoteFormResponse::Cancel,
        })
    }
}

fn remote_workspace_root(workspace_root: &std::path::Path) -> Result<McpRemoteRoot> {
    let canonical = workspace_root
        .canonicalize()
        .context("failed to canonicalize workspace root for remote MCP roots")?;
    let uri = Url::from_directory_path(&canonical)
        .map_err(|()| anyhow!("workspace root cannot be represented as a file URI"))?
        .to_string();
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("workspace")
        .to_owned();
    McpRemoteRoot::new(uri, name).map_err(Into::into)
}

struct RemoteMcpTool {
    client: Arc<McpStreamableHttpClient>,
    remote_tool: McpRemoteTool,
    provider_name: String,
    server_name: String,
    trust: sigil_kernel::McpServerTrustPolicy,
    lifecycle_owner: ToolLifecycleOwner,
    redactor: sigil_kernel::SecretRedactor,
    attempts: Arc<RuntimeMcpTransportAttemptFactory>,
    execution_lock: Arc<tokio::sync::Mutex<()>>,
}

#[async_trait]
impl Tool for RemoteMcpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.provider_name.clone(),
            description: self
                .remote_tool
                .description
                .clone()
                .unwrap_or_else(|| format!("Remote MCP tool {}", self.remote_tool.name)),
            input_schema: self.remote_tool.input_schema.clone(),
            category: ToolCategory::Mcp,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Read),
            preview: ToolPreviewCapability::None,
        }
    }

    fn lifecycle_owner(&self) -> Option<ToolLifecycleOwner> {
        Some(self.lifecycle_owner.clone())
    }

    fn permission_operation(&self, _ctx: &ToolContext, _args: &Value) -> Result<ToolOperation> {
        Ok(ToolOperation::NetworkRequest)
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        Ok(vec![
            ToolSubject::mcp_tool(self.provider_name.clone()),
            ToolSubject::mcp_trust_class(self.server_name.clone(), self.trust.trust_class.as_str()),
        ])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(Some(self.trust.approval_default))
    }

    fn egress_audit(&self, _ctx: &ToolContext, args: &Value) -> Result<Option<ToolEgressAudit>> {
        if !self.trust.egress_logging {
            return Ok(None);
        }
        let keys = args
            .as_object()
            .map(|object| object.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        Ok(Some(ToolEgressAudit {
            destination: format!("mcp:{}", self.server_name),
            operation: format!("tools/call:{}", self.remote_tool.name),
            payload: json!({ "argument_keys": keys, "redacted": true }),
            redacted: true,
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        let Some(budget) = ctx.web_task_tree_budget() else {
            return Ok(ToolResult::error(
                call_id,
                &self.provider_name,
                ToolErrorKind::Internal,
                "remote MCP tool requires the current root-owned task-tree budget",
            ));
        };
        let _execution_guard = self.execution_lock.lock().await;
        if self.attempts.rebind_budget(budget).is_err() {
            return Ok(ToolResult::error(
                call_id,
                &self.provider_name,
                ToolErrorKind::Internal,
                "remote MCP tool could not bind the current task-tree budget",
            ));
        }
        let cancellation = ctx.cancellation_handle();
        let result = self
            .client
            .call_tool(&self.remote_tool, args, cancellation.as_ref(), &|| true)
            .await;
        match result {
            Ok(result) if !result.is_error => {
                let raw = json!({
                    "content": result.content,
                    "structured_content": result.structured_content,
                });
                let encoded = serde_json::to_string(&raw)?;
                let safe = safe_persistence_text(&self.redactor.redact_text(&encoded));
                let metadata = ToolResultMeta {
                    details: json!({
                        "provenance": "external_untrusted",
                        "transport": "streamable_http",
                        "server": self.server_name,
                    }),
                    ..ToolResultMeta::default()
                };
                Ok(ToolResult::ok(call_id, &self.provider_name, safe, metadata))
            }
            Ok(_) => Ok(ToolResult::error(
                call_id,
                &self.provider_name,
                ToolErrorKind::Protocol,
                "remote MCP tool returned isError=true",
            )),
            Err(error) => Ok(ToolResult::error(
                call_id,
                &self.provider_name,
                ToolErrorKind::Protocol,
                format!("remote MCP tool failed: {error}"),
            )),
        }
    }

    async fn shutdown(&self) -> Result<()> {
        self.client.close(true).await?;
        Ok(())
    }
}

pub(crate) fn proxy_environment(root: &RootConfig) -> ProxyEnvironment {
    if root.web.proxy_mode == sigil_kernel::WebProxyMode::Direct {
        return ProxyEnvironment::default();
    }
    let value = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| std::env::var(name).ok())
            .map(SecretString::new)
    };
    let no_proxy = ["NO_PROXY", "no_proxy"]
        .iter()
        .find_map(|name| std::env::var(name).ok());
    ProxyEnvironment::from_values(
        value(&["HTTP_PROXY", "http_proxy"]),
        value(&["HTTPS_PROXY", "https_proxy"]),
        value(&["ALL_PROXY", "all_proxy"]),
        no_proxy.as_deref(),
    )
}

pub(crate) fn destination_policy(root: &RootConfig) -> Result<WebDestinationGuardPolicy> {
    let mut policy = WebDestinationGuardPolicy::default()
        .with_allowed_ports(root.web.allowed_ports.iter().copied())
        .with_blocked_domains(root.web.blocked_domains.iter().cloned());
    let cidrs = root
        .web
        .allowed_private_cidrs
        .iter()
        .map(|value| {
            let (address, prefix) = value
                .split_once('/')
                .ok_or_else(|| anyhow!("invalid private CIDR {value}"))?;
            IpCidr::new(address.parse::<IpAddr>()?, prefix.parse::<u8>()?).map_err(Into::into)
        })
        .collect::<Result<Vec<_>>>()?;
    for host in &root.web.allowed_private_hosts {
        policy = policy.with_private_exception(host.clone(), cidrs.clone());
    }
    Ok(policy)
}

pub(crate) fn enforce_allowed_domain(root: &RootConfig, endpoint: &Url) -> Result<()> {
    if root.web.allowed_domains.is_empty() {
        return Ok(());
    }
    let host = endpoint
        .host_str()
        .ok_or_else(|| anyhow!("remote MCP endpoint has no host"))?
        .to_ascii_lowercase();
    if root.web.allowed_domains.iter().any(|allowed| {
        let allowed = allowed.trim_start_matches("*.").to_ascii_lowercase();
        host == allowed || host.ends_with(&format!(".{allowed}"))
    }) {
        Ok(())
    } else {
        bail!("remote MCP endpoint is outside web.allowed_domains")
    }
}

pub(crate) fn web_budget_limits(root: &RootConfig) -> WebTaskTreeBudgetLimits {
    WebTaskTreeBudgetLimits {
        max_fetch_calls: u64::from(root.web.max_fetches_per_run.max(1)),
        max_client_search_calls: u64::from(root.web.max_client_searches_per_run.max(1)),
        max_hosted_requests: u64::from(
            root.web.max_hosted_enabled_provider_requests_per_run.max(1),
        ),
        max_network_attempts: u64::from(root.web.max_network_attempts_per_run.max(1)),
        max_wire_bytes: root.web.max_total_wire_bytes_per_run.max(1),
        max_decoded_bytes: root.web.max_total_decoded_bytes_per_run.max(1),
        max_model_bytes: root.web.max_total_model_bytes_per_run.max(1),
        max_concurrent_requests: u64::from(root.web.max_concurrent_requests.max(1)),
        max_attempts_per_host: u64::from(root.web.per_host_rate_limit_per_minute.max(1)),
    }
}

async fn validate_remote_pin(
    server: &McpServerConfig,
    transport_fingerprint: &str,
    client: &McpStreamableHttpClient,
) -> Result<()> {
    if !server.trust.pin_version {
        return Ok(());
    }
    let expected = server
        .trust
        .pinned
        .as_ref()
        .ok_or_else(|| anyhow!("pin_version requires a pinned identity"))?;
    let identity = client
        .server_identity()
        .await
        .ok_or_else(|| anyhow!("remote MCP server identity is unavailable"))?;
    let protocol = client
        .protocol_version()
        .await
        .ok_or_else(|| anyhow!("remote MCP protocol version is unavailable"))?;
    if expected.transport_fingerprint != transport_fingerprint
        || expected.protocol_version != protocol.as_str()
        || expected.server_name != identity.name
        || expected.server_version != identity.version
    {
        bail!("remote MCP pinned identity mismatch");
    }
    Ok(())
}

fn sha256_fingerprint(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}
