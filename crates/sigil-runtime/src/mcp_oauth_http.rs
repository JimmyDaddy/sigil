use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::{
    Client, Method, Proxy,
    header::{CONTENT_LENGTH, CONTENT_TYPE, HeaderName, HeaderValue},
    redirect::Policy,
};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    EgressBindingOrigin, EgressDataCategory, EgressDisclosurePresenter, EgressNetworkRoute,
    RootConfig, SecretString, WebBudgetByteKind, WebBudgetReservation, WebTaskTreeBudget,
};
use sigil_mcp::{
    McpOAuthHttpExecutor, McpOAuthHttpMethod, McpOAuthHttpPurpose, McpOAuthHttpRequest,
    McpOAuthHttpResponse, McpOAuthTransportError,
};
use sigil_tools_builtin::WebFetchRoute;
use url::Url;

use crate::{
    EgressOrderingCoordinator, RuntimeMcpStreamableHttpAttemptFactory,
    RuntimeMcpTransportAttemptFactory, SystemWebDestinationResolver, WebDestinationGuard,
    remote_mcp::{destination_policy, enforce_allowed_domain, proxy_environment},
};

const MAX_OAUTH_RESPONSE_BYTES: usize = 128 * 1024;

/// Runtime-owned OAuth transport. Every request gets an independent durable authorization,
/// disclosure, budget reservation, destination guard and physical client.
pub struct RuntimeMcpOAuthHttpExecutor {
    root_config: RootConfig,
    guard: Arc<WebDestinationGuard<SystemWebDestinationResolver>>,
    ordering: EgressOrderingCoordinator,
    budget: Arc<WebTaskTreeBudget>,
    admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
    timeout: Duration,
}

impl std::fmt::Debug for RuntimeMcpOAuthHttpExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeMcpOAuthHttpExecutor")
            .field("root_run_id", &self.budget.root_run_id())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl RuntimeMcpOAuthHttpExecutor {
    pub fn new(
        root_config: &RootConfig,
        recorder: sigil_kernel::EgressAuditRecorder,
        presenter: Arc<dyn EgressDisclosurePresenter>,
        budget: Arc<WebTaskTreeBudget>,
        admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> anyhow::Result<Self> {
        let guard = Arc::new(WebDestinationGuard::new(
            SystemWebDestinationResolver,
            destination_policy(root_config)?,
            proxy_environment(root_config),
        ));
        Ok(Self {
            root_config: root_config.clone(),
            guard,
            ordering: EgressOrderingCoordinator::new(recorder, Some(presenter)),
            budget,
            admission_is_live,
            timeout: Duration::from_secs(root_config.web.timeout_secs.max(1)),
        })
    }

    async fn authorize(
        &self,
        destination: &Url,
        purpose: McpOAuthHttpPurpose,
    ) -> Result<
        (
            sigil_tools_builtin::WebFetchAuthorizedDialPlan,
            WebBudgetReservation,
        ),
        McpOAuthTransportError,
    > {
        enforce_allowed_domain(&self.root_config, destination)
            .map_err(|_| McpOAuthTransportError::DestinationRejected)?;
        let preview = self
            .guard
            .preview(destination.clone())
            .map_err(|_| McpOAuthTransportError::DestinationRejected)?;
        let route = if preview.is_proxy_remote() {
            EgressNetworkRoute::ProxyRemote
        } else {
            EgressNetworkRoute::Direct
        };
        let purpose_label = purpose_label(purpose);
        let route_fingerprint = sha256_fingerprint(&format!(
            "mcp-oauth\0{purpose_label}\0{}",
            preview.safe_logical_destination()
        ));
        let profile_fingerprint = sha256_fingerprint(&format!(
            "{}\0{}\0{}",
            route_fingerprint,
            self.root_config.web.proxy_mode as u8,
            preview.safe_transport_destination()
        ));
        let attempt_factory = RuntimeMcpTransportAttemptFactory::new(
            Arc::clone(&self.budget),
            self.budget.root_run_id(),
            EgressBindingOrigin::UserConfigured,
            format!("remote-mcp-oauth-{purpose_label}-v1"),
            "mcp_oauth",
            format!("MCP OAuth {purpose_label}"),
            route_fingerprint,
            profile_fingerprint,
            preview.safe_logical_destination(),
            preview.safe_transport_destination(),
            route,
            vec![EgressDataCategory::ConnectionMetadata],
        );
        let attempt = attempt_factory
            .next_attempt()
            .await
            .map_err(|error| match error {
                sigil_mcp::McpStreamableHttpDestinationError::BudgetExhausted => {
                    McpOAuthTransportError::BudgetExhausted
                }
                sigil_mcp::McpStreamableHttpDestinationError::PreEgressRejected
                | sigil_mcp::McpStreamableHttpDestinationError::DestinationRejected => {
                    McpOAuthTransportError::DestinationRejected
                }
            })?;
        let (authorization, disclosure, reservation, attempt_id) = attempt.into_parts();
        let permit = self
            .ordering
            .authorize_transport(
                authorization,
                disclosure,
                reservation,
                self.admission_is_live.as_ref(),
            )
            .await
            .map_err(map_ordering_error)?;
        let budget = permit
            .begin_attempt(&attempt_id, preview.safe_host())
            .map_err(map_ordering_error)?;
        let plan = self
            .guard
            .authorize_preview(preview)
            .await
            .map_err(|_| McpOAuthTransportError::DestinationRejected)?;
        Ok((plan, budget))
    }

    fn build_client(
        &self,
        plan: &sigil_tools_builtin::WebFetchAuthorizedDialPlan,
        destination: &Url,
    ) -> Result<Client, McpOAuthTransportError> {
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .pool_max_idle_per_host(0)
            .referer(false)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd();
        match plan.route() {
            WebFetchRoute::Direct => {
                let host = destination
                    .host_str()
                    .ok_or(McpOAuthTransportError::DestinationRejected)?;
                builder = builder.resolve_to_addrs(host, plan.direct_addresses());
            }
            WebFetchRoute::EnvironmentProxy => {
                let proxy = plan
                    .proxy_url()
                    .ok_or(McpOAuthTransportError::DestinationRejected)?;
                builder = builder.proxy(
                    Proxy::all(proxy.expose_secret())
                        .map_err(|_| McpOAuthTransportError::DestinationRejected)?,
                );
            }
        }
        builder
            .build()
            .map_err(|_| McpOAuthTransportError::Transport)
    }
}

/// Builds one isolated OAuth user-action executor and task-tree budget.
pub fn runtime_mcp_oauth_executor_for_user_action(
    root_config: &RootConfig,
    recorder: sigil_kernel::EgressAuditRecorder,
    presenter: Arc<dyn EgressDisclosurePresenter>,
    admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
) -> anyhow::Result<Arc<RuntimeMcpOAuthHttpExecutor>> {
    let budget = sigil_kernel::WebTaskTreeBudget::new(
        format!("remote-mcp-oauth-action-{}", uuid::Uuid::new_v4()),
        crate::remote_mcp::web_budget_limits(root_config),
        None,
    )?;
    Ok(Arc::new(RuntimeMcpOAuthHttpExecutor::new(
        root_config,
        recorder,
        presenter,
        budget,
        admission_is_live,
    )?))
}

#[async_trait]
impl McpOAuthHttpExecutor for RuntimeMcpOAuthHttpExecutor {
    async fn execute(
        &self,
        request: McpOAuthHttpRequest,
    ) -> Result<McpOAuthHttpResponse, McpOAuthTransportError> {
        let destination = validate_oauth_destination(request.destination())?;
        let (plan, mut budget) = self.authorize(&destination, request.purpose()).await?;
        let client = self.build_client(&plan, &destination)?;
        let method = match request.method() {
            McpOAuthHttpMethod::Get => Method::GET,
            McpOAuthHttpMethod::Post => Method::POST,
        };
        let mut outgoing = client.request(method, destination);
        outgoing = outgoing.header("accept", "application/json");
        if let Some(content_type) = request.content_type() {
            outgoing = outgoing.header(CONTENT_TYPE, content_type);
        }
        for (name, value) in request.headers() {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| McpOAuthTransportError::Transport)?;
            let value = HeaderValue::from_str(value.expose_secret())
                .map_err(|_| McpOAuthTransportError::Transport)?;
            outgoing = outgoing.header(name, value);
        }
        if let Some(body) = request.body() {
            budget
                .charge_chunk(WebBudgetByteKind::Wire, body.len() as u64)
                .map_err(|_| McpOAuthTransportError::BudgetExhausted)?;
            outgoing = outgoing.body(body.to_owned());
        }
        let response = tokio::time::timeout(self.timeout, outgoing.send())
            .await
            .map_err(|_| McpOAuthTransportError::Transport)?
            .map_err(|_| McpOAuthTransportError::Transport)?;
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<usize>().ok())
            .is_some_and(|value| value > MAX_OAUTH_RESPONSE_BYTES)
        {
            return Err(McpOAuthTransportError::Transport);
        }
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let mut body = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| McpOAuthTransportError::Transport)?;
            budget
                .charge_chunk(WebBudgetByteKind::Wire, chunk.len() as u64)
                .and_then(|()| budget.charge_chunk(WebBudgetByteKind::Decoded, chunk.len() as u64))
                .map_err(|_| McpOAuthTransportError::BudgetExhausted)?;
            if body.len().saturating_add(chunk.len()) > MAX_OAUTH_RESPONSE_BYTES {
                return Err(McpOAuthTransportError::Transport);
            }
            body.extend_from_slice(&chunk);
        }
        let body = String::from_utf8(body).map_err(|_| McpOAuthTransportError::Transport)?;
        Ok(McpOAuthHttpResponse::new(
            status,
            content_type,
            SecretString::new(body),
        ))
    }
}

fn validate_oauth_destination(value: &str) -> Result<Url, McpOAuthTransportError> {
    let destination = Url::parse(value).map_err(|_| McpOAuthTransportError::DestinationRejected)?;
    if destination.scheme() != "https"
        || destination.host_str().is_none()
        || !destination.username().is_empty()
        || destination.password().is_some()
        || destination.fragment().is_some()
    {
        return Err(McpOAuthTransportError::DestinationRejected);
    }
    Ok(destination)
}

fn map_ordering_error(error: crate::EgressOrderingError) -> McpOAuthTransportError {
    match error {
        crate::EgressOrderingError::Budget(_) => McpOAuthTransportError::BudgetExhausted,
        _ => McpOAuthTransportError::DestinationRejected,
    }
}

fn purpose_label(purpose: McpOAuthHttpPurpose) -> &'static str {
    match purpose {
        McpOAuthHttpPurpose::ProtectedResourceMetadata => "resource-discovery",
        McpOAuthHttpPurpose::AuthorizationServerMetadata => "issuer-discovery",
        McpOAuthHttpPurpose::DynamicClientRegistration => "client-registration",
        McpOAuthHttpPurpose::TokenExchange => "token-exchange",
        McpOAuthHttpPurpose::TokenRefresh => "token-refresh",
        McpOAuthHttpPurpose::TokenRevocation => "token-revocation",
    }
}

fn sha256_fingerprint(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
#[path = "tests/mcp_oauth_http_tests.rs"]
mod tests;
