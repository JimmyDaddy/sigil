use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sigil_kernel::{
    PreEgressDisclosure, QueryEgressStarted, QueryEgressTerminalStatus, RunCancellationHandle,
    SecretRedactor, SecretString, WebBudgetByteKind, WebBudgetReservation, WebSearchFailureClass,
    safe_persistence_text, strip_terminal_control_sequences,
};
use sigil_mcp::{
    KnownMcpSearchAdapter, McpCallToolResult, McpRemoteClientCapabilities, McpRemoteServerIdentity,
    McpRemoteTool, McpRequestBodyObserver, McpSearchAdapterKind, McpStableSearchEligibility,
    McpStreamableHttpAuthState, McpStreamableHttpClient, McpStreamableHttpDestinationAuthorizer,
    McpStreamableHttpDestinationError, McpStreamableHttpError, McpStreamableHttpHeaderConfig,
    McpStreamableHttpHeaderEnvironment, McpStreamableHttpLimits, PreparedMcpStreamableHttpHeaders,
    classify_mcp_search_binding, mcp_schema_fingerprint, mcp_tool_schema_fingerprint,
};
use tokio::sync::OnceCell;

use crate::{
    AuthorizedQueryEgress, EgressOrderingCoordinator, EgressOrderingError,
    McpSearchBindingRegistry, PreparedMcpSearchLease, SourceProjection,
    SourceProjectionUnavailableReason, WebSearchConnector, WebSearchConnectorError,
    WebSearchConnectorIdentity, WebSearchFailure, WebSearchProtocolFailureKind, WebSearchRequest,
    WebSearchResponse, exa_text_v1::EXA_TEXT_V1_CODEC_ID, exa_text_v1::decode_exa_text_v1,
};

const BUNDLED_PROFILE_ID: &str = "builtin:exa-anonymous";
const BUNDLED_DISCLOSURE_ID: &str = "exa-anonymous-2026-06-29";
const BUNDLED_ENDPOINT: &str = "https://mcp.exa.ai/mcp";
const BUNDLED_SERVER_NAME: &str = "exa-search-server";
const BUNDLED_SERVER_VERSION: &str = "3.2.1";
const BUNDLED_TOOL_NAME: &str = "web_search_exa";
const BUNDLED_ADAPTER_ID: &str = "exa-web-search-3.2.1";
const MAX_RESULT_COUNT: u32 = 10;

pub struct RuntimeStableSearchQueryAttempt {
    pub disclosure: PreEgressDisclosure,
    pub started: QueryEgressStarted,
    pub reservation: WebBudgetReservation,
}

#[async_trait]
pub trait StableSearchQueryAttemptFactory: Send + Sync {
    async fn next_attempt(
        &self,
        request: &WebSearchRequest,
        identity: &WebSearchConnectorIdentity,
    ) -> Result<RuntimeStableSearchQueryAttempt, WebSearchConnectorError>;
}

#[async_trait]
trait StableSearchQueryPermitFactory: Send + Sync {
    async fn authorize(
        &self,
        request: &WebSearchRequest,
        identity: &WebSearchConnectorIdentity,
    ) -> Result<AuthorizedQueryEgress, WebSearchConnectorError>;
}

pub struct RuntimeStableSearchQueryPermitFactory {
    ordering: EgressOrderingCoordinator,
    attempts: Arc<dyn StableSearchQueryAttemptFactory>,
    admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl RuntimeStableSearchQueryPermitFactory {
    #[must_use]
    pub fn new(
        ordering: EgressOrderingCoordinator,
        attempts: Arc<dyn StableSearchQueryAttemptFactory>,
        admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            ordering,
            attempts,
            admission_is_live,
        }
    }
}

#[async_trait]
impl StableSearchQueryPermitFactory for RuntimeStableSearchQueryPermitFactory {
    async fn authorize(
        &self,
        request: &WebSearchRequest,
        identity: &WebSearchConnectorIdentity,
    ) -> Result<AuthorizedQueryEgress, WebSearchConnectorError> {
        let attempt = self.attempts.next_attempt(request, identity).await?;
        self.ordering
            .authorize_query(
                attempt.disclosure,
                attempt.started,
                attempt.reservation,
                self.admission_is_live.as_ref(),
            )
            .await
            .map_err(map_ordering_failure)
    }
}

#[async_trait]
pub trait BundledExaAuthorizerFactory: Send + Sync {
    async fn create(
        &self,
        endpoint: SecretString,
        profile_config_proxy_fingerprint: String,
        live_header_fingerprint: String,
    ) -> Result<Arc<dyn McpStreamableHttpDestinationAuthorizer>, McpStreamableHttpError>;
}

struct EmptyHeaderEnvironment;

impl McpStreamableHttpHeaderEnvironment for EmptyHeaderEnvironment {
    fn resolve(&self, _name: &str) -> Option<SecretString> {
        None
    }
}

#[async_trait]
trait StableMcpSearchTransportFactory: Send + Sync {
    async fn connect(&self) -> Result<Arc<dyn StableMcpSearchTransport>, McpStreamableHttpError>;
}

struct RuntimeBundledExaTransportFactory {
    authorizers: Arc<dyn BundledExaAuthorizerFactory>,
}

#[async_trait]
impl StableMcpSearchTransportFactory for RuntimeBundledExaTransportFactory {
    async fn connect(&self) -> Result<Arc<dyn StableMcpSearchTransport>, McpStreamableHttpError> {
        let endpoint = SecretString::new(BUNDLED_ENDPOINT);
        let prepared = PreparedMcpStreamableHttpHeaders::prepare(
            endpoint.clone(),
            &McpStreamableHttpHeaderConfig::default(),
            &EmptyHeaderEnvironment,
        )?;
        let authorizer = self
            .authorizers
            .create(
                endpoint,
                bundled_profile_fingerprint(),
                prepared.live_header_fingerprint().to_owned(),
            )
            .await?;
        let client = McpStreamableHttpClient::connect_prepared(
            authorizer,
            prepared,
            McpRemoteClientCapabilities::empty(),
            McpStreamableHttpLimits::default(),
        )
        .await?;
        Ok(client)
    }
}

#[async_trait]
trait StableMcpSearchTransport: Send + Sync {
    fn auth_state(&self) -> McpStreamableHttpAuthState;
    fn transport_fingerprint(&self) -> String;
    fn live_header_fingerprint(&self) -> String;
    fn profile_config_proxy_fingerprint(&self) -> String;
    async fn server_identity(&self) -> Option<McpRemoteServerIdentity>;
    async fn list_tools(&self) -> Result<Vec<McpRemoteTool>, McpStreamableHttpError>;
    async fn call_tool(
        &self,
        tool: &McpRemoteTool,
        arguments: Value,
        cancellation: Option<&RunCancellationHandle>,
        observer: Arc<dyn McpRequestBodyObserver>,
    ) -> Result<McpCallToolResult, McpStreamableHttpError>;
}

#[async_trait]
impl StableMcpSearchTransport for McpStreamableHttpClient {
    fn auth_state(&self) -> McpStreamableHttpAuthState {
        self.auth_state()
    }

    fn transport_fingerprint(&self) -> String {
        self.transport_fingerprint()
    }

    fn live_header_fingerprint(&self) -> String {
        self.live_header_fingerprint().to_owned()
    }

    fn profile_config_proxy_fingerprint(&self) -> String {
        self.profile_config_proxy_fingerprint()
    }

    async fn server_identity(&self) -> Option<McpRemoteServerIdentity> {
        self.server_identity().await
    }

    async fn list_tools(&self) -> Result<Vec<McpRemoteTool>, McpStreamableHttpError> {
        self.list_tools().await
    }

    async fn call_tool(
        &self,
        tool: &McpRemoteTool,
        arguments: Value,
        cancellation: Option<&RunCancellationHandle>,
        observer: Arc<dyn McpRequestBodyObserver>,
    ) -> Result<McpCallToolResult, McpStreamableHttpError> {
        self.call_tool_with_body_observer(tool, arguments, cancellation, &|| true, Some(observer))
            .await
    }
}

pub(crate) struct BundledExaSearchConnector {
    transports: Arc<dyn StableMcpSearchTransportFactory>,
    transport: OnceCell<Arc<dyn StableMcpSearchTransport>>,
    permits: Arc<dyn StableSearchQueryPermitFactory>,
    redactor: SecretRedactor,
    session_scope_id: String,
}

impl BundledExaSearchConnector {
    pub(crate) fn new(
        authorizers: Arc<dyn BundledExaAuthorizerFactory>,
        permits: Arc<RuntimeStableSearchQueryPermitFactory>,
        redactor: SecretRedactor,
        session_scope_id: impl Into<String>,
    ) -> Self {
        Self {
            transports: Arc::new(RuntimeBundledExaTransportFactory { authorizers }),
            transport: OnceCell::new(),
            permits,
            redactor,
            session_scope_id: session_scope_id.into(),
        }
    }

    async fn transport(
        &self,
    ) -> Result<Arc<dyn StableMcpSearchTransport>, WebSearchConnectorError> {
        self.transport
            .get_or_try_init(|| async { self.transports.connect().await.map_err(map_mcp_failure) })
            .await
            .cloned()
    }
}

#[async_trait]
impl WebSearchConnector for BundledExaSearchConnector {
    fn identity(&self) -> WebSearchConnectorIdentity {
        bundled_identity()
    }

    async fn search(
        &self,
        request: WebSearchRequest,
    ) -> Result<WebSearchResponse, WebSearchConnectorError> {
        validate_request(&request, &self.redactor, true)?;
        let transport = self.transport().await?;
        validate_bundled_transport(transport.as_ref()).await?;
        let tool = find_and_validate_bundled_tool(transport.as_ref()).await?;
        let identity = self.identity();
        let permit = self.permits.authorize(&request, &identity).await?;
        let observer = Arc::new(QueryBodyObserver::new(permit));
        let arguments = json!({
            "query": request.query.expose_secret(),
            "numResults": request.max_results,
        });
        let result = transport
            .call_tool(
                &tool,
                arguments,
                request.cancellation.as_ref(),
                Arc::clone(&observer) as Arc<dyn McpRequestBodyObserver>,
            )
            .await;
        match result {
            Ok(result) => {
                let raw = match exact_text_result(result) {
                    Ok(raw) => raw,
                    Err(class) => {
                        observer.finish(QueryEgressTerminalStatus::Failed, Some(class))?;
                        return Err(failed(class));
                    }
                };
                let response = decode_exa_text_v1(
                    &raw,
                    &self.session_scope_id,
                    &request.retrieved_at,
                    &self.redactor,
                );
                observer.charge_model(response.safe_model_content.len() as u64)?;
                observer.finish(QueryEgressTerminalStatus::Completed, None)?;
                Ok(response)
            }
            Err(error) => {
                let failure = observer
                    .barrier_failure()
                    .map_or_else(|| mcp_failure(&error), WebSearchFailure::new);
                let status = if failure.class == WebSearchFailureClass::RateLimited {
                    QueryEgressTerminalStatus::RateLimited
                } else if failure.class == WebSearchFailureClass::Cancelled {
                    QueryEgressTerminalStatus::Cancelled
                } else {
                    QueryEgressTerminalStatus::Failed
                };
                observer.finish(status, Some(failure.class))?;
                Err(WebSearchConnectorError::Failed(failure))
            }
        }
    }
}

pub(crate) struct ConfiguredStableMcpSearchConnector {
    registry: Arc<McpSearchBindingRegistry>,
    lease: PreparedMcpSearchLease,
    transport: Arc<dyn StableMcpSearchTransport>,
    permits: Arc<dyn StableSearchQueryPermitFactory>,
    redactor: SecretRedactor,
    session_scope_id: String,
}

impl ConfiguredStableMcpSearchConnector {
    pub(crate) fn new(
        registry: Arc<McpSearchBindingRegistry>,
        lease: PreparedMcpSearchLease,
        transport: Arc<McpStreamableHttpClient>,
        permits: Arc<RuntimeStableSearchQueryPermitFactory>,
        redactor: SecretRedactor,
        session_scope_id: impl Into<String>,
    ) -> Self {
        Self {
            registry,
            lease,
            transport,
            permits,
            redactor,
            session_scope_id: session_scope_id.into(),
        }
    }
}

#[async_trait]
impl WebSearchConnector for ConfiguredStableMcpSearchConnector {
    fn identity(&self) -> WebSearchConnectorIdentity {
        WebSearchConnectorIdentity {
            origin: self.lease.binding.origin.clone(),
            safe_destination: self.lease.binding.safe_destination.clone(),
            server_identity_fingerprint: self.lease.binding.server_identity_fingerprint.clone(),
            tool_schema_fingerprint: self.lease.binding.tool_schema_fingerprint.clone(),
            codec_id: match &self.lease.binding.adapter {
                McpSearchAdapterKind::KnownVersioned { codec_id, .. } => codec_id.clone(),
                McpSearchAdapterKind::GenericQueryText => None,
            },
            disclosure_id: None,
        }
    }

    async fn search(
        &self,
        request: WebSearchRequest,
    ) -> Result<WebSearchResponse, WebSearchConnectorError> {
        validate_request(&request, &self.redactor, false)?;
        self.registry
            .validate_lease(&self.lease)
            .map_err(|_| failed(WebSearchFailureClass::ConfigurationInvalid))?;
        if self.transport.transport_fingerprint() != self.lease.binding.transport_fingerprint
            || self.transport.live_header_fingerprint()
                != self.lease.binding.live_header_fingerprint
            || self.transport.profile_config_proxy_fingerprint()
                != self.lease.binding.profile_config_proxy_fingerprint
        {
            return Err(failed(WebSearchFailureClass::ConfigurationInvalid));
        }
        let identity = self
            .transport
            .server_identity()
            .await
            .ok_or_else(|| failed(WebSearchFailureClass::IdentityMismatch))?;
        if identity.fingerprint != self.lease.binding.server_identity_fingerprint {
            return Err(failed(WebSearchFailureClass::IdentityMismatch));
        }
        let tools = self.transport.list_tools().await.map_err(map_mcp_failure)?;
        let tool = tools
            .into_iter()
            .find(|tool| tool.name == self.lease.binding.tool_name)
            .ok_or_else(|| failed(WebSearchFailureClass::SchemaDrift))?;
        if mcp_tool_schema_fingerprint(&tool) != self.lease.binding.tool_schema_fingerprint {
            return Err(failed(WebSearchFailureClass::SchemaDrift));
        }
        self.registry
            .validate_lease(&self.lease)
            .map_err(|_| failed(WebSearchFailureClass::ConfigurationInvalid))?;
        let connector_identity = self.identity();
        let permit = self
            .permits
            .authorize(&request, &connector_identity)
            .await?;
        let observer = Arc::new(QueryBodyObserver::new(permit));
        let arguments = match self.lease.binding.adapter {
            McpSearchAdapterKind::GenericQueryText => {
                json!({ "query": request.query.expose_secret() })
            }
            McpSearchAdapterKind::KnownVersioned { .. } => json!({
                "query": request.query.expose_secret(),
                "numResults": request.max_results,
            }),
        };
        let result = self
            .transport
            .call_tool(
                &tool,
                arguments,
                request.cancellation.as_ref(),
                Arc::clone(&observer) as Arc<dyn McpRequestBodyObserver>,
            )
            .await;
        match result {
            Ok(result) => {
                let response = match &self.lease.binding.adapter {
                    McpSearchAdapterKind::KnownVersioned { codec_id, .. }
                        if codec_id.as_deref() == Some(EXA_TEXT_V1_CODEC_ID) =>
                    {
                        let raw = match exact_text_result(result) {
                            Ok(raw) => raw,
                            Err(class) => {
                                observer.finish(QueryEgressTerminalStatus::Failed, Some(class))?;
                                return Err(failed(class));
                            }
                        };
                        decode_exa_text_v1(
                            &raw,
                            &self.session_scope_id,
                            &request.retrieved_at,
                            &self.redactor,
                        )
                    }
                    _ => {
                        let safe_model_content = match generic_result_text(result, &self.redactor) {
                            Ok(content) => content,
                            Err(class) => {
                                observer.finish(QueryEgressTerminalStatus::Failed, Some(class))?;
                                return Err(failed(class));
                            }
                        };
                        WebSearchResponse {
                            safe_model_content,
                            sources: Vec::new(),
                            source_projection: SourceProjection::Unavailable {
                                reason: SourceProjectionUnavailableReason::GenericAdapterNoSourceContract,
                            },
                        }
                    }
                };
                observer.charge_model(response.safe_model_content.len() as u64)?;
                observer.finish(QueryEgressTerminalStatus::Completed, None)?;
                Ok(response)
            }
            Err(error) => {
                let failure = observer
                    .barrier_failure()
                    .map_or_else(|| mcp_failure(&error), WebSearchFailure::new);
                let status = if failure.class == WebSearchFailureClass::RateLimited {
                    QueryEgressTerminalStatus::RateLimited
                } else if failure.class == WebSearchFailureClass::Cancelled {
                    QueryEgressTerminalStatus::Cancelled
                } else {
                    QueryEgressTerminalStatus::Failed
                };
                observer.finish(status, Some(failure.class))?;
                Err(WebSearchConnectorError::Failed(failure))
            }
        }
    }
}

enum QueryBodyState {
    Authorized(AuthorizedQueryEgress),
    Active(crate::ActiveQueryEgress),
    Terminal,
}

struct QueryBodyObserver {
    state: Mutex<Option<QueryBodyState>>,
    barrier_failure: Mutex<Option<WebSearchFailureClass>>,
}

impl QueryBodyObserver {
    fn new(permit: AuthorizedQueryEgress) -> Self {
        Self {
            state: Mutex::new(Some(QueryBodyState::Authorized(permit))),
            barrier_failure: Mutex::new(None),
        }
    }

    fn barrier_failure(&self) -> Option<WebSearchFailureClass> {
        self.barrier_failure
            .lock()
            .ok()
            .and_then(|failure| *failure)
    }

    fn finish(
        &self,
        status: QueryEgressTerminalStatus,
        error: Option<WebSearchFailureClass>,
    ) -> Result<(), WebSearchConnectorError> {
        let state = self
            .state
            .lock()
            .map_err(|_| failed(WebSearchFailureClass::UnexpectedResponse))?
            .take();
        match state {
            Some(QueryBodyState::Authorized(permit)) => permit
                .finish_without_body(status, error)
                .map_err(map_ordering_failure),
            Some(QueryBodyState::Active(active)) => {
                active.finish(status, error).map_err(map_ordering_failure)
            }
            Some(QueryBodyState::Terminal) | None => Ok(()),
        }
    }

    fn charge_model(&self, bytes: u64) -> Result<(), WebSearchConnectorError> {
        let charge = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| failed(WebSearchFailureClass::UnexpectedResponse))?;
            let Some(QueryBodyState::Active(active)) = state.as_mut() else {
                return Err(failed(WebSearchFailureClass::UnexpectedResponse));
            };
            active.charge_chunk(WebBudgetByteKind::Model, bytes)
        };
        if let Err(error) = charge {
            let class = ordering_failure_class(&error);
            self.finish(QueryEgressTerminalStatus::Failed, Some(class))?;
            return Err(failed(class));
        };
        Ok(())
    }
}

impl McpRequestBodyObserver for QueryBodyObserver {
    fn on_first_body_poll(&self) -> Result<(), McpStreamableHttpError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| McpStreamableHttpError::Transport)?;
        let Some(QueryBodyState::Authorized(permit)) = state.take() else {
            return Err(McpStreamableHttpError::InvalidLifecycle);
        };
        match permit.begin_body() {
            Ok(active) => {
                *state = Some(QueryBodyState::Active(active));
                Ok(())
            }
            Err(error) => {
                if let Ok(mut failure) = self.barrier_failure.lock() {
                    *failure = Some(ordering_failure_class(&error));
                }
                *state = Some(QueryBodyState::Terminal);
                Err(McpStreamableHttpError::BudgetExhausted)
            }
        }
    }
}

async fn validate_bundled_transport(
    transport: &dyn StableMcpSearchTransport,
) -> Result<(), WebSearchConnectorError> {
    if transport.auth_state() != McpStreamableHttpAuthState::Anonymous
        || transport.transport_fingerprint() != bundled_profile_fingerprint()
    {
        return Err(failed(WebSearchFailureClass::ConfigurationInvalid));
    }
    let identity = transport
        .server_identity()
        .await
        .ok_or_else(|| failed(WebSearchFailureClass::IdentityMismatch))?;
    if identity.name != BUNDLED_SERVER_NAME
        || identity.version != BUNDLED_SERVER_VERSION
        || identity.fingerprint != bundled_server_identity_fingerprint()
    {
        return Err(failed(WebSearchFailureClass::IdentityMismatch));
    }
    Ok(())
}

async fn find_and_validate_bundled_tool(
    transport: &dyn StableMcpSearchTransport,
) -> Result<McpRemoteTool, WebSearchConnectorError> {
    let tool = transport
        .list_tools()
        .await
        .map_err(map_mcp_failure)?
        .into_iter()
        .find(|tool| tool.name == BUNDLED_TOOL_NAME)
        .ok_or_else(|| failed(WebSearchFailureClass::SchemaDrift))?;
    let expected = bundled_known_adapter();
    match classify_mcp_search_binding(&bundled_server_identity_fingerprint(), &tool, &[expected]) {
        McpStableSearchEligibility::Eligible(McpSearchAdapterKind::KnownVersioned {
            adapter_id,
            codec_id,
        }) if adapter_id == BUNDLED_ADAPTER_ID
            && codec_id.as_deref() == Some(EXA_TEXT_V1_CODEC_ID) =>
        {
            Ok(tool)
        }
        _ => Err(failed(WebSearchFailureClass::SchemaDrift)),
    }
}

fn bundled_known_adapter() -> KnownMcpSearchAdapter {
    KnownMcpSearchAdapter {
        adapter_id: BUNDLED_ADAPTER_ID.to_owned(),
        codec_id: Some(EXA_TEXT_V1_CODEC_ID.to_owned()),
        server_identity_fingerprint: bundled_server_identity_fingerprint(),
        tool_name: BUNDLED_TOOL_NAME.to_owned(),
        input_schema_fingerprint: mcp_schema_fingerprint(&bundled_input_schema()),
        output_schema_fingerprint: None,
    }
}

fn bundled_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "minLength": 1,
                "description": "Natural language search query. Should be a semantically rich description of the ideal page, not just keywords. Optionally include category:<type> (company, people) to focus results — e.g. 'category:people John Doe software engineer'."
            },
            "numResults": {
                "type": "number",
                "description": "Number of search results to return (default: 10)."
            }
        },
        "required": ["query"],
        "additionalProperties": false,
        "$schema": "http://json-schema.org/draft-07/schema#"
    })
}

fn bundled_identity() -> WebSearchConnectorIdentity {
    let tool = McpRemoteTool {
        name: BUNDLED_TOOL_NAME.to_owned(),
        description: None,
        input_schema: bundled_input_schema(),
        output_schema: None,
        task_support: Some("forbidden".to_owned()),
    };
    WebSearchConnectorIdentity {
        origin: crate::McpSearchBindingOrigin::Bundled {
            profile_id: BUNDLED_PROFILE_ID.to_owned(),
            disclosure_id: BUNDLED_DISCLOSURE_ID.to_owned(),
        },
        safe_destination: "https://mcp.exa.ai/".to_owned(),
        server_identity_fingerprint: bundled_server_identity_fingerprint(),
        tool_schema_fingerprint: mcp_tool_schema_fingerprint(&tool),
        codec_id: Some(EXA_TEXT_V1_CODEC_ID.to_owned()),
        disclosure_id: Some(BUNDLED_DISCLOSURE_ID.to_owned()),
    }
}

fn validate_request(
    request: &WebSearchRequest,
    redactor: &SecretRedactor,
    bundled: bool,
) -> Result<(), WebSearchConnectorError> {
    let query = request.query.expose_secret();
    let normalized = crate::normalize_web_search_query(query, redactor, bundled)?;
    if request.correlation_id.is_empty()
        || request.retrieved_at.is_empty()
        || request.retrieved_at.len() > 35
        || request.retrieved_at.as_bytes().get(10) != Some(&b'T')
        || normalized.query.expose_secret() != query
        || request.query_chars != query.chars().count()
        || request.query_bytes != query.len()
        || request.max_results == 0
        || request.max_results > MAX_RESULT_COUNT
    {
        return Err(failed(WebSearchFailureClass::InvalidInput));
    }
    Ok(())
}

fn exact_text_result(result: McpCallToolResult) -> Result<String, WebSearchFailureClass> {
    if result.is_error || result.structured_content.is_some() || result.content.len() != 1 {
        return Err(if result.is_error {
            WebSearchFailureClass::ToolExecutionFailed
        } else {
            WebSearchFailureClass::UnexpectedResponse
        });
    }
    let block = result
        .content
        .into_iter()
        .next()
        .and_then(|value| value.as_object().cloned())
        .ok_or(WebSearchFailureClass::UnexpectedResponse)?;
    if block.get("type").and_then(Value::as_str) != Some("text") {
        return Err(WebSearchFailureClass::UnexpectedResponse);
    }
    block
        .get("text")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or(WebSearchFailureClass::UnexpectedResponse)
}

fn generic_result_text(
    result: McpCallToolResult,
    redactor: &SecretRedactor,
) -> Result<String, WebSearchFailureClass> {
    if result.is_error {
        return Err(WebSearchFailureClass::ToolExecutionFailed);
    }
    let mut segments = Vec::new();
    for block in result.content {
        let Some(block) = block.as_object() else {
            return Err(WebSearchFailureClass::UnexpectedResponse);
        };
        if block.get("type").and_then(Value::as_str) == Some("text") {
            let text = block
                .get("text")
                .and_then(Value::as_str)
                .ok_or(WebSearchFailureClass::UnexpectedResponse)?;
            let safe = safe_plain_text(text, redactor);
            if !safe.is_empty() {
                segments.push(safe);
            }
        }
    }
    if let Some(structured) = result.structured_content {
        let serialized = serde_json::to_string(&structured)
            .map_err(|_| WebSearchFailureClass::UnexpectedResponse)?;
        let safe = safe_plain_text(&serialized, redactor);
        if !safe.is_empty() {
            segments.push(safe);
        }
    }
    if segments.is_empty() {
        Err(WebSearchFailureClass::UnexpectedResponse)
    } else {
        Ok(safe_plain_text(&segments.join("\n\n"), redactor))
    }
}

fn safe_plain_text(value: &str, redactor: &SecretRedactor) -> String {
    let stripped = strip_terminal_control_sequences(value);
    let redacted = redactor.redact_text(&stripped);
    let projected = safe_persistence_text(&redacted);
    let mut end = projected.len().min(256 * 1024);
    while !projected.is_char_boundary(end) {
        end -= 1;
    }
    projected[..end].to_owned()
}

fn bundled_profile_fingerprint() -> String {
    sha256(&format!(
        "{BUNDLED_PROFILE_ID}\0{BUNDLED_ENDPOINT}\0{BUNDLED_DISCLOSURE_ID}\0{BUNDLED_ADAPTER_ID}\0{EXA_TEXT_V1_CODEC_ID}"
    ))
}

fn bundled_server_identity_fingerprint() -> String {
    sha256(&format!("{BUNDLED_SERVER_NAME}\0{BUNDLED_SERVER_VERSION}"))
}

fn sha256(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn failed(class: WebSearchFailureClass) -> WebSearchConnectorError {
    WebSearchConnectorError::Failed(WebSearchFailure::new(class))
}

fn map_ordering_failure(error: EgressOrderingError) -> WebSearchConnectorError {
    failed(ordering_failure_class(&error))
}

fn ordering_failure_class(error: &EgressOrderingError) -> WebSearchFailureClass {
    match error {
        EgressOrderingError::Budget(_) => WebSearchFailureClass::BudgetExhausted,
        EgressOrderingError::AdmissionRevoked => WebSearchFailureClass::Cancelled,
        EgressOrderingError::MissingPresenter | EgressOrderingError::Presentation(_) => {
            WebSearchFailureClass::DisclosureFailed
        }
        EgressOrderingError::BindingMismatch | EgressOrderingError::Audit(_) => {
            WebSearchFailureClass::UnexpectedResponse
        }
    }
}

fn map_mcp_failure(error: McpStreamableHttpError) -> WebSearchConnectorError {
    WebSearchConnectorError::Failed(mcp_failure(&error))
}

fn mcp_failure(error: &McpStreamableHttpError) -> WebSearchFailure {
    use McpStreamableHttpError as E;
    match error {
        E::AuthenticationRequired => {
            WebSearchFailure::new(WebSearchFailureClass::AuthenticationRequired)
        }
        E::AuthenticationFailed => {
            WebSearchFailure::new(WebSearchFailureClass::AuthenticationFailed)
        }
        E::OAuthUnsupported => WebSearchFailure::new(WebSearchFailureClass::OAuthUnsupported),
        E::AccessDenied => WebSearchFailure::new(WebSearchFailureClass::AccessDenied),
        E::RateLimited => WebSearchFailure::new(WebSearchFailureClass::RateLimited),
        E::SessionExpired => WebSearchFailure::new(WebSearchFailureClass::SessionExpired),
        E::SchemaDrift => WebSearchFailure::new(WebSearchFailureClass::SchemaDrift),
        E::Timeout => WebSearchFailure::new(WebSearchFailureClass::Timeout),
        E::Cancelled => WebSearchFailure::new(WebSearchFailureClass::Cancelled),
        E::ServiceUnavailable => WebSearchFailure::new(WebSearchFailureClass::ServiceUnavailable),
        E::BudgetExhausted
        | E::DestinationAuthorization(McpStreamableHttpDestinationError::BudgetExhausted) => {
            WebSearchFailure::new(WebSearchFailureClass::BudgetExhausted)
        }
        E::DestinationAuthorization(McpStreamableHttpDestinationError::PreEgressRejected) => {
            WebSearchFailure::new(WebSearchFailureClass::DisclosureFailed)
        }
        E::DestinationAuthorization(McpStreamableHttpDestinationError::DestinationRejected) => {
            WebSearchFailure::new(WebSearchFailureClass::PolicyDenied)
        }
        E::Transport => WebSearchFailure::new(WebSearchFailureClass::TransportUnavailable),
        E::UnexpectedHttpStatus { status } => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::UnexpectedHttpStatus {
                status: *status,
            })
        }
        E::JsonRpcError { code } => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::JsonRpcError { code: *code })
        }
        E::ResponseIdMismatch => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::ResponseIdMismatch)
        }
        E::UnsupportedProtocolVersion => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::UnsupportedProtocolVersion)
        }
        E::InitializedNotificationRejected => WebSearchFailure::protocol(
            WebSearchProtocolFailureKind::InitializedNotificationRejected,
        ),
        E::InvalidSessionId => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::InvalidSessionId)
        }
        E::MissingToolsCapability => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::MissingToolsCapability)
        }
        E::InvalidPagination => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::InvalidPagination)
        }
        E::UnexpectedContentType => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::UnexpectedContentType)
        }
        E::MalformedEnvelope => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::MalformedEnvelope)
        }
        E::MissingRequiredContent => {
            WebSearchFailure::protocol(WebSearchProtocolFailureKind::MissingRequiredContent)
        }
        E::InvalidEndpoint
        | E::InvalidDialPlan
        | E::InvalidSafeDestination
        | E::InvalidLimits
        | E::ConfigurationInvalid => {
            WebSearchFailure::new(WebSearchFailureClass::ConfigurationInvalid)
        }
        E::InvalidLifecycle
        | E::HeaderLimitExceeded
        | E::BodyLimitExceeded
        | E::SseLimitExceeded
        | E::InvalidForm
        | E::UrlElicitationUnsupported
        | E::CapabilityNotNegotiated
        | E::InvalidAuthenticationChallenge => {
            WebSearchFailure::new(WebSearchFailureClass::UnexpectedResponse)
        }
    }
}

#[cfg(test)]
#[path = "tests/stable_mcp_search_tests.rs"]
mod tests;
