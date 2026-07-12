use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ApprovalMode, DEFAULT_WEB_URL_CAPABILITY_TTL_MS, EgressBindingOrigin, EgressDataCategory,
    EgressDisclosureKind, EgressDisclosurePresenter, EgressNetworkRoute, NetworkEffect,
    NetworkPolicy, PreEgressDisclosure, QueryEgressStarted, QueryEgressTerminalStatus, RootConfig,
    SecretString, Tool, ToolAccess, ToolCategory, ToolContext, ToolEgressAudit, ToolErrorKind,
    ToolOperation, ToolPreviewCapability, ToolRegistry, ToolResult, ToolResultMeta, ToolSpec,
    ToolSubject, UserUrlCapabilityRegistration, WebBudgetReservationKind,
    WebBudgetReservationRequest, WebQueryEgressClass, WebSearchFailureClass, WebSearchMcpConfig,
    WebSearchRoute, WebTaskTreeBudget, WebUrlProvenanceKind,
};
use sigil_mcp::{
    McpRemoteTool, McpSearchAdapterKind, McpStableSearchEligibility, classify_mcp_search_binding,
    mcp_provider_tool_name_candidate,
};
use url::Url;

use crate::stable_mcp_search::{BUNDLED_SEARCH_ENDPOINT, BundledExaSearchConnector};
use crate::{
    BundledExaAuthorizerFactory, EgressOrderingCoordinator,
    RuntimeMcpStreamableHttpDestinationAuthorizer, RuntimeMcpTransportAttemptFactory,
    RuntimeStableSearchQueryAttempt, RuntimeStableSearchQueryPermitFactory,
    StableSearchQueryAttemptFactory, WebDestinationGuard, WebSearchConnector,
    WebSearchConnectorError, WebSearchConnectorIdentity, WebSearchRequest,
    normalize_web_search_query, secret_redactor_for_root_config,
};

pub(crate) fn register_web_search_tool(
    registry: &mut ToolRegistry,
    root_config: &RootConfig,
    provider_tool_name_max_chars: usize,
    presenter: Arc<dyn EgressDisclosurePresenter>,
) {
    if !root_config.web.enabled
        || root_config.web.search_route == WebSearchRoute::Disabled
        || root_config.web.search_route == WebSearchRoute::ProviderHosted
    {
        return;
    }
    registry.register(Arc::new(WebSearchTool {
        registry: registry.clone(),
        root_config: root_config.clone(),
        provider_tool_name_max_chars,
        presenter,
    }));
}

struct WebSearchTool {
    registry: ToolRegistry,
    root_config: RootConfig,
    provider_tool_name_max_chars: usize,
    presenter: Arc<dyn EgressDisclosurePresenter>,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "websearch".to_owned(),
            description: "Search the web through the configured stable search route. Results include bounded external/untrusted snippets that should be used directly when they answer the task. Do not automatically fan out to webfetch across results; fetch a page only when the user explicitly asks to read it or one specific missing fact cannot be answered from the snippets. Search queries are disclosed before egress.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "The exact web search query." },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 10 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            category: ToolCategory::Search,
            access: ToolAccess::Read,
            network_effect: Some(NetworkEffect::Read),
            preview: ToolPreviewCapability::None,
        }
    }

    fn permission_operation(&self, _ctx: &ToolContext, _args: &Value) -> Result<ToolOperation> {
        Ok(ToolOperation::NetworkRequest)
    }

    fn permission_subjects(&self, _ctx: &ToolContext, _args: &Value) -> Result<Vec<ToolSubject>> {
        if let Some(binding) = &self.root_config.web.search_mcp {
            let provider_name = mcp_provider_tool_name_candidate(
                &binding.server,
                &binding.tool,
                self.provider_tool_name_max_chars,
            );
            let mut subjects = vec![ToolSubject::mcp_tool(provider_name)];
            if let Some(server) = self
                .root_config
                .mcp_servers
                .iter()
                .find(|server| server.name == binding.server)
            {
                subjects.push(ToolSubject::mcp_trust_class(
                    server.name.clone(),
                    server.trust.trust_class.as_str(),
                ));
            }
            return Ok(subjects);
        }
        Ok(vec![ToolSubject::mcp_tool("builtin:exa-anonymous")])
    }

    fn permission_default_mode(
        &self,
        _ctx: &ToolContext,
        _args: &Value,
    ) -> Result<Option<ApprovalMode>> {
        Ok(self
            .root_config
            .web
            .search_mcp
            .as_ref()
            .and_then(|binding| {
                self.root_config
                    .mcp_servers
                    .iter()
                    .find(|server| server.name == binding.server)
            })
            .map(|server| server.trust.approval_default))
    }

    fn egress_audit(&self, _ctx: &ToolContext, args: &Value) -> Result<Option<ToolEgressAudit>> {
        let route = self
            .root_config
            .web
            .search_mcp
            .as_ref()
            .map_or("bundled_exa", |_| "configured_mcp");
        Ok(Some(ToolEgressAudit {
            destination: route.to_owned(),
            operation: "web/search".to_owned(),
            payload: json!({
                "route": route,
                "query_chars": args.get("query").and_then(Value::as_str).map(str::chars).map(Iterator::count),
            }),
            redacted: true,
        }))
    }

    async fn execute(&self, ctx: ToolContext, call_id: String, args: Value) -> Result<ToolResult> {
        if ctx.network_policy() == NetworkPolicy::Deny
            || (ctx.network_policy() == NetworkPolicy::Ask && !ctx.explicit_network_approval())
        {
            return Ok(ToolResult::error(
                call_id,
                "websearch",
                ToolErrorKind::PermissionDenied,
                "web search requires current network authorization",
            ));
        }
        let raw_query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("websearch query must be a string"))?;
        let redactor = secret_redactor_for_root_config(&self.root_config);
        let bundled = self.root_config.web.search_mcp.is_none();
        let normalized = normalize_web_search_query(raw_query, &redactor, bundled)
            .map_err(|_| anyhow!("websearch query is invalid or blocked by egress policy"))?;
        if normalized.chars > self.root_config.web.max_query_chars
            || normalized.bytes > self.root_config.web.max_query_bytes
        {
            return Ok(ToolResult::error(
                call_id,
                "websearch",
                ToolErrorKind::InvalidInput,
                "websearch query exceeds configured limits",
            ));
        }
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(self.root_config.web.max_results))
            .min(u64::from(self.root_config.web.max_results))
            .min(10);
        if max_results == 0 {
            return Ok(ToolResult::error(
                call_id,
                "websearch",
                ToolErrorKind::InvalidInput,
                "websearch max_results must be positive",
            ));
        }
        let request = WebSearchRequest {
            correlation_id: format!("websearch-{call_id}"),
            query: normalized.query,
            query_chars: normalized.chars,
            query_bytes: normalized.bytes,
            provenance: WebQueryEgressClass::ModelGenerated,
            max_results: u32::try_from(max_results).unwrap_or(10),
            retrieved_at: current_rfc3339(),
            cancellation: ctx.cancellation_handle(),
        };
        if self.root_config.web.search_mcp.is_some() {
            return self.execute_configured(ctx, call_id, request).await;
        }
        self.execute_bundled(ctx, call_id, request, redactor).await
    }
}

impl WebSearchTool {
    async fn execute_bundled(
        &self,
        ctx: ToolContext,
        call_id: String,
        request: WebSearchRequest,
        redactor: sigil_kernel::SecretRedactor,
    ) -> Result<ToolResult> {
        if !self.root_config.web.bundled_search.enabled
            || self.root_config.web.search_route == WebSearchRoute::Mcp
        {
            return Ok(unavailable(
                call_id,
                WebSearchFailureClass::ConfigurationInvalid,
            ));
        }
        let recorder = ctx
            .egress_audit_recorder()
            .ok_or_else(|| anyhow!("websearch requires a durable session recorder"))?;
        let budget = ctx
            .web_task_tree_budget()
            .ok_or_else(|| anyhow!("websearch requires a root-owned task-tree budget"))?;
        let session_scope_id = ctx
            .session_scope_id()
            .ok_or_else(|| anyhow!("websearch requires an active logical session scope"))?
            .to_owned();
        let root_run_id = budget.root_run_id().to_owned();
        let query_destination = match query_egress_destination(
            &self.root_config,
            Url::parse(BUNDLED_SEARCH_ENDPOINT).expect("bundled search endpoint is a valid URL"),
        ) {
            Ok(destination) => destination,
            Err(_) => {
                return Ok(unavailable(
                    call_id,
                    WebSearchFailureClass::ConfigurationInvalid,
                ));
            }
        };
        let cancellation = ctx.cancellation_handle();
        let admission_is_live: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            cancellation
                .as_ref()
                .is_none_or(|handle| !handle.is_cancel_requested())
        });
        let attempts = Arc::new(RuntimeQueryAttemptFactory {
            budget: Arc::clone(&budget),
            root_run_id: root_run_id.clone(),
            surface: "tui_or_cli".to_owned(),
            safe_transport_destination: query_destination.safe_transport_destination,
            route: query_destination.route,
        });
        let permits = Arc::new(RuntimeStableSearchQueryPermitFactory::new(
            EgressOrderingCoordinator::new(recorder.clone(), Some(Arc::clone(&self.presenter))),
            attempts,
            Arc::clone(&admission_is_live),
        ));
        let authorizers = Arc::new(RuntimeBundledAuthorizerFactory {
            root_config: self.root_config.clone(),
            recorder,
            presenter: Arc::clone(&self.presenter),
            budget,
            root_run_id: root_run_id.clone(),
            admission_is_live,
        });
        let connector = BundledExaSearchConnector::new(
            authorizers,
            permits,
            redactor,
            session_scope_id.clone(),
        );
        match connector.search(request).await {
            Ok(response) => search_result(call_id, response, &session_scope_id),
            Err(error) => Ok(connector_error(call_id, error)),
        }
    }

    async fn execute_configured(
        &self,
        ctx: ToolContext,
        call_id: String,
        request: WebSearchRequest,
    ) -> Result<ToolResult> {
        let binding = self
            .root_config
            .web
            .search_mcp
            .as_ref()
            .expect("configured route checked");
        let query_destination =
            match configured_query_egress_destination(&self.root_config, binding) {
                Ok(destination) => destination,
                Err(_) => {
                    return Ok(unavailable(
                        call_id,
                        WebSearchFailureClass::ConfigurationInvalid,
                    ));
                }
            };
        let provider_name = mcp_provider_tool_name_candidate(
            &binding.server,
            &binding.tool,
            self.provider_tool_name_max_chars,
        );
        if self.registry.spec_for(&provider_name).is_none() {
            let activation = sigil_kernel::ToolCall {
                id: format!("{call_id}-activate"),
                name: "mcp_activate_server".to_owned(),
                args_json: serde_json::to_string(&json!({"server_name": binding.server}))?,
            };
            let result = self.registry.execute(ctx.clone(), activation).await?;
            if !matches!(result.status, sigil_kernel::ToolResultStatus::Ok) {
                return Ok(unavailable(
                    call_id,
                    WebSearchFailureClass::ConfigurationInvalid,
                ));
            }
        }
        let Some(spec) = self.registry.spec_for(&provider_name) else {
            return Ok(unavailable(call_id, WebSearchFailureClass::SchemaDrift));
        };
        let descriptor = McpRemoteTool {
            name: binding.tool.clone(),
            description: Some(spec.description),
            input_schema: spec.input_schema,
            output_schema: None,
            task_support: None,
        };
        if !matches!(
            classify_mcp_search_binding("configured", &descriptor, &[]),
            McpStableSearchEligibility::Eligible(McpSearchAdapterKind::GenericQueryText)
        ) {
            return Ok(unavailable(call_id, WebSearchFailureClass::SchemaDrift));
        }
        let recorder = ctx
            .egress_audit_recorder()
            .ok_or_else(|| anyhow!("websearch requires a durable session recorder"))?;
        let budget = ctx
            .web_task_tree_budget()
            .ok_or_else(|| anyhow!("websearch requires a root-owned task-tree budget"))?;
        let root_run_id = budget.root_run_id().to_owned();
        let attempts = RuntimeQueryAttemptFactory {
            budget,
            root_run_id,
            surface: "tui_or_cli".to_owned(),
            safe_transport_destination: query_destination.safe_transport_destination,
            route: query_destination.route,
        };
        let identity = WebSearchConnectorIdentity {
            origin: crate::McpSearchBindingOrigin::UserConfigured,
            safe_destination: query_destination.safe_logical_destination,
            server_identity_fingerprint: sha256(&format!("server:{}", binding.server)),
            tool_schema_fingerprint: sigil_mcp::mcp_tool_schema_fingerprint(&descriptor),
            codec_id: None,
            disclosure_id: None,
        };
        let attempt = match attempts.next_attempt(&request, &identity).await {
            Ok(attempt) => attempt,
            Err(error) => return Ok(connector_error(call_id, error)),
        };
        let cancellation = ctx.cancellation_handle();
        let permit = EgressOrderingCoordinator::new(recorder, Some(Arc::clone(&self.presenter)))
            .authorize_query(
                attempt.disclosure,
                attempt.started,
                attempt.reservation,
                &move || {
                    cancellation
                        .as_ref()
                        .is_none_or(|handle| !handle.is_cancel_requested())
                },
            )
            .await?;
        let active = permit.begin_body()?;
        let nested = self
            .registry
            .execute(
                ctx,
                sigil_kernel::ToolCall {
                    id: format!("{call_id}-configured"),
                    name: provider_name,
                    args_json: serde_json::to_string(&json!({
                        "query": request.query.expose_secret()
                    }))?,
                },
            )
            .await;
        match nested {
            Ok(result) if matches!(result.status, sigil_kernel::ToolResultStatus::Ok) => {
                active.finish(QueryEgressTerminalStatus::Completed, None)?;
                let mut metadata = result.metadata;
                metadata.details = json!({
                    "provenance": "external_untrusted",
                    "route": "configured_mcp",
                    "source_projection": "unavailable_generic_adapter",
                });
                Ok(ToolResult::ok(
                    call_id,
                    "websearch",
                    result.content,
                    metadata,
                ))
            }
            Ok(_) | Err(_) => {
                active.finish(
                    QueryEgressTerminalStatus::Failed,
                    Some(WebSearchFailureClass::ToolExecutionFailed),
                )?;
                Ok(unavailable(
                    call_id,
                    WebSearchFailureClass::ToolExecutionFailed,
                ))
            }
        }
    }
}

struct QueryEgressDestination {
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: EgressNetworkRoute,
}

fn configured_query_egress_destination(
    root_config: &RootConfig,
    binding: &WebSearchMcpConfig,
) -> Result<QueryEgressDestination> {
    let server = root_config
        .mcp_servers
        .iter()
        .find(|server| server.name == binding.server)
        .ok_or_else(|| anyhow!("configured websearch MCP server is missing"))?;
    let remote = server
        .streamable_http()
        .ok_or_else(|| anyhow!("configured websearch MCP server must use streamable_http"))?;
    let endpoint = Url::parse(&remote.url)
        .map_err(|_| anyhow!("configured websearch MCP endpoint is invalid"))?;
    crate::remote_mcp::enforce_allowed_domain(root_config, &endpoint)?;
    query_egress_destination(root_config, endpoint)
}

fn query_egress_destination(
    root_config: &RootConfig,
    endpoint: Url,
) -> Result<QueryEgressDestination> {
    query_egress_destination_with_proxy(
        root_config,
        endpoint,
        crate::remote_mcp::proxy_environment(root_config),
    )
}

fn query_egress_destination_with_proxy(
    root_config: &RootConfig,
    endpoint: Url,
    proxy_environment: crate::ProxyEnvironment,
) -> Result<QueryEgressDestination> {
    let guard = WebDestinationGuard::new(
        crate::SystemWebDestinationResolver,
        crate::remote_mcp::destination_policy(root_config)?,
        proxy_environment,
    );
    let preview = guard.preview(endpoint)?;
    Ok(QueryEgressDestination {
        safe_logical_destination: preview.safe_logical_destination().to_owned(),
        safe_transport_destination: preview.safe_transport_destination().to_owned(),
        route: if preview.is_proxy_remote() {
            EgressNetworkRoute::ProxyRemote
        } else {
            EgressNetworkRoute::Direct
        },
    })
}

struct RuntimeQueryAttemptFactory {
    budget: Arc<WebTaskTreeBudget>,
    root_run_id: String,
    surface: String,
    safe_transport_destination: String,
    route: EgressNetworkRoute,
}

#[async_trait]
impl StableSearchQueryAttemptFactory for RuntimeQueryAttemptFactory {
    async fn next_attempt(
        &self,
        request: &WebSearchRequest,
        identity: &WebSearchConnectorIdentity,
    ) -> Result<RuntimeStableSearchQueryAttempt, WebSearchConnectorError> {
        let unique = uuid::Uuid::new_v4();
        let route_lease_id = format!("websearch-lease-{unique}");
        let route_fingerprint = sha256(&format!(
            "{}\0{}\0{}",
            identity.safe_destination,
            identity.server_identity_fingerprint,
            identity.tool_schema_fingerprint
        ));
        let reservation = self
            .budget
            .reserve(WebBudgetReservationRequest {
                correlation_id: request.correlation_id.clone(),
                attempt_id: format!("websearch-attempt-{unique}"),
                route_lease_id: route_lease_id.clone(),
                route_fingerprint: route_fingerprint.clone(),
                kind: WebBudgetReservationKind::ClientSearchCall,
            })
            .map_err(|_| connector_failure(WebSearchFailureClass::BudgetExhausted))?;
        let profile_fingerprint = sha256(&format!(
            "websearch-profile\0{route_fingerprint}\0{}\0{:?}",
            self.safe_transport_destination, self.route
        ));
        let disclosure = PreEgressDisclosure::new(
            EgressDisclosureKind::Query,
            Some(request.correlation_id.clone()),
            format!("websearch-query-{unique}"),
            self.surface.clone(),
            "Web search query",
            route_fingerprint.clone(),
            profile_fingerprint,
            identity.safe_destination.clone(),
            self.safe_transport_destination.clone(),
            self.route,
            vec![EgressDataCategory::SearchQuery],
        )
        .map_err(|_| connector_failure(WebSearchFailureClass::DisclosureFailed))?;
        Ok(RuntimeStableSearchQueryAttempt {
            disclosure,
            started: QueryEgressStarted {
                record_id: format!("websearch-query-start-{unique}"),
                root_run_id: self.root_run_id.clone(),
                correlation_id: request.correlation_id.clone(),
                route_lease_id,
                route_fingerprint,
                query_chars: request.query_chars,
                query_bytes: request.query_bytes,
                egress_class: request.provenance,
            },
            reservation,
        })
    }
}

struct RuntimeBundledAuthorizerFactory {
    root_config: RootConfig,
    recorder: sigil_kernel::EgressAuditRecorder,
    presenter: Arc<dyn EgressDisclosurePresenter>,
    budget: Arc<WebTaskTreeBudget>,
    root_run_id: String,
    admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
}

#[async_trait]
impl BundledExaAuthorizerFactory for RuntimeBundledAuthorizerFactory {
    async fn create(
        &self,
        endpoint: SecretString,
        profile_config_proxy_fingerprint: String,
        live_header_fingerprint: String,
    ) -> Result<
        Arc<dyn sigil_mcp::McpStreamableHttpDestinationAuthorizer>,
        sigil_mcp::McpStreamableHttpError,
    > {
        let parsed = Url::parse(endpoint.expose_secret())
            .map_err(|_| sigil_mcp::McpStreamableHttpError::ConfigurationInvalid)?;
        let guard = Arc::new(WebDestinationGuard::new(
            crate::SystemWebDestinationResolver,
            crate::remote_mcp::destination_policy(&self.root_config)
                .map_err(|_| sigil_mcp::McpStreamableHttpError::ConfigurationInvalid)?,
            crate::remote_mcp::proxy_environment(&self.root_config),
        ));
        let preview = guard.preview(parsed).map_err(|_| {
            sigil_mcp::McpStreamableHttpError::DestinationAuthorization(
                sigil_mcp::McpStreamableHttpDestinationError::DestinationRejected,
            )
        })?;
        let route = if preview.is_proxy_remote() {
            EgressNetworkRoute::ProxyRemote
        } else {
            EgressNetworkRoute::Direct
        };
        let attempts = Arc::new(RuntimeMcpTransportAttemptFactory::new(
            Arc::clone(&self.budget),
            self.root_run_id.clone(),
            EgressBindingOrigin::BundledReleaseProfile,
            "bundled-exa-transport-v1",
            "mcp",
            "Anonymous Exa MCP transport",
            profile_config_proxy_fingerprint.clone(),
            profile_config_proxy_fingerprint.clone(),
            preview.safe_logical_destination(),
            preview.safe_transport_destination(),
            route,
            vec![EgressDataCategory::ConnectionMetadata],
        ));
        Ok(Arc::new(
            RuntimeMcpStreamableHttpDestinationAuthorizer::new(
                endpoint,
                guard,
                EgressOrderingCoordinator::new(
                    self.recorder.clone(),
                    Some(Arc::clone(&self.presenter)),
                ),
                attempts,
                profile_config_proxy_fingerprint,
                live_header_fingerprint,
                Arc::clone(&self.admission_is_live),
            )
            .with_transport_fingerprint(crate::stable_mcp_search::bundled_profile_fingerprint()),
        ))
    }
}

fn search_result(
    call_id: String,
    response: crate::WebSearchResponse,
    session_scope_id: &str,
) -> Result<ToolResult> {
    if response
        .sources
        .iter()
        .any(|source| source.session_scope_id != session_scope_id)
    {
        return Err(anyhow!(
            "websearch source projection does not belong to the active session scope"
        ));
    }
    let issued_at_ms = crate::current_unix_time_ms();
    let registrations = response
        .source_capabilities
        .into_iter()
        .map(|capability| {
            let replayable_canonical_url = (capability.restart_policy
                == sigil_kernel::ToolRestartPolicy::Replayable)
                .then(|| capability.raw_canonical_url.expose_secret().to_owned());
            UserUrlCapabilityRegistration {
                source_id: capability.source_id,
                durable_entry_id: call_id.clone(),
                raw_canonical_url: capability.raw_canonical_url,
                safe_display_url: capability.safe_display_url,
                restart_policy: capability.restart_policy,
                replayable_canonical_url,
                originating_call_id: Some(call_id.clone()),
                provenance: WebUrlProvenanceKind::WebSearchResult,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(DEFAULT_WEB_URL_CAPABILITY_TTL_MS),
            }
        })
        .collect();
    let sources = response.sources;
    let metadata = ToolResultMeta {
        bytes: Some(response.safe_model_content.len() as u64),
        details: json!({
            "provenance": "external_untrusted",
            "route": "bundled_exa",
            "sources": sources.clone(),
            "source_projection": format!("{:?}", response.source_projection),
        }),
        ..ToolResultMeta::default()
    };
    Ok(
        ToolResult::ok(call_id, "websearch", response.safe_model_content, metadata)
            .with_url_capability_registrations(registrations)
            .with_external_sources(sources),
    )
}

fn connector_error(call_id: String, error: WebSearchConnectorError) -> ToolResult {
    let class = match error {
        WebSearchConnectorError::Failed(failure) => failure.class,
        WebSearchConnectorError::InvalidFailureContract => {
            WebSearchFailureClass::UnexpectedResponse
        }
    };
    unavailable(call_id, class)
}

fn unavailable(call_id: String, class: WebSearchFailureClass) -> ToolResult {
    let kind = match class {
        WebSearchFailureClass::InvalidInput => ToolErrorKind::InvalidInput,
        WebSearchFailureClass::PolicyDenied
        | WebSearchFailureClass::ApprovalRejected
        | WebSearchFailureClass::SecretBlocked
        | WebSearchFailureClass::SensitivePersonalDataBlocked => ToolErrorKind::PermissionDenied,
        WebSearchFailureClass::ProtocolError
        | WebSearchFailureClass::SchemaDrift
        | WebSearchFailureClass::IdentityMismatch
        | WebSearchFailureClass::UnexpectedResponse => ToolErrorKind::Protocol,
        _ => ToolErrorKind::Network,
    };
    ToolResult::error(
        call_id,
        "websearch",
        kind,
        format!("websearch route unavailable: {class:?}"),
    )
}

fn connector_failure(class: WebSearchFailureClass) -> WebSearchConnectorError {
    WebSearchConnectorError::Failed(crate::WebSearchFailure::new(class))
}

fn sha256(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

pub(crate) fn current_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        day_seconds / 3_600,
        (day_seconds % 3_600) / 60,
        day_seconds % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
#[path = "tests/web_search_tool_tests.rs"]
mod tests;
