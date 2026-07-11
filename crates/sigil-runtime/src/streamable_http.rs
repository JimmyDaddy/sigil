use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use sigil_kernel::{
    McpTransportAuthorization, PreEgressDisclosure, SecretString, WebBudgetReservation,
};
use sigil_mcp::{
    McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationAuthorizer,
    McpStreamableHttpDestinationError,
};
use sigil_tools_builtin::WebFetchRoute;
use url::Url;

use crate::{
    EgressOrderingCoordinator, EgressOrderingError, WebDestinationGuard, WebDestinationResolver,
};

/// Inputs for one independently authorized Streamable HTTP message attempt.
pub struct RuntimeMcpStreamableHttpAttempt {
    authorization: McpTransportAuthorization,
    disclosure: PreEgressDisclosure,
    reservation: WebBudgetReservation,
    attempt_id: String,
}

impl RuntimeMcpStreamableHttpAttempt {
    #[must_use]
    pub fn new(
        authorization: McpTransportAuthorization,
        disclosure: PreEgressDisclosure,
        reservation: WebBudgetReservation,
        attempt_id: impl Into<String>,
    ) -> Self {
        Self {
            authorization,
            disclosure,
            reservation,
            attempt_id: attempt_id.into(),
        }
    }
}

/// Produces a fresh durable authorization/disclosure/budget tuple for every HTTP message.
#[async_trait]
pub trait RuntimeMcpStreamableHttpAttemptFactory: Send + Sync {
    async fn next_attempt(
        &self,
    ) -> Result<RuntimeMcpStreamableHttpAttempt, McpStreamableHttpDestinationError>;
}

/// Internal deterministic factory used by conformance fixtures and later runtime assembly.
pub struct QueuedRuntimeMcpStreamableHttpAttemptFactory {
    attempts: Mutex<VecDeque<RuntimeMcpStreamableHttpAttempt>>,
}

impl QueuedRuntimeMcpStreamableHttpAttemptFactory {
    #[must_use]
    pub fn new(attempts: impl IntoIterator<Item = RuntimeMcpStreamableHttpAttempt>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into_iter().collect()),
        }
    }
}

#[async_trait]
impl RuntimeMcpStreamableHttpAttemptFactory for QueuedRuntimeMcpStreamableHttpAttemptFactory {
    async fn next_attempt(
        &self,
    ) -> Result<RuntimeMcpStreamableHttpAttempt, McpStreamableHttpDestinationError> {
        self.attempts
            .lock()
            .map_err(|_| McpStreamableHttpDestinationError::PreEgressRejected)?
            .pop_front()
            .ok_or(McpStreamableHttpDestinationError::PreEgressRejected)
    }
}

/// Reusable runtime adapter that enforces E21.8 durable ordering before invoking the shared E21.9
/// destination guard for each HTTP message. It is not wired into root config or the MCP registry.
pub struct RuntimeMcpStreamableHttpDestinationAuthorizer<R> {
    endpoint: SecretString,
    guard: Arc<WebDestinationGuard<R>>,
    ordering: EgressOrderingCoordinator,
    attempt_factory: Arc<dyn RuntimeMcpStreamableHttpAttemptFactory>,
    profile_config_proxy_fingerprint: String,
    live_header_fingerprint: String,
    admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl<R> std::fmt::Debug for RuntimeMcpStreamableHttpDestinationAuthorizer<R> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeMcpStreamableHttpDestinationAuthorizer")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl<R> RuntimeMcpStreamableHttpDestinationAuthorizer<R>
where
    R: WebDestinationResolver,
{
    #[must_use]
    pub fn new(
        endpoint: SecretString,
        guard: Arc<WebDestinationGuard<R>>,
        ordering: EgressOrderingCoordinator,
        attempt_factory: Arc<dyn RuntimeMcpStreamableHttpAttemptFactory>,
        profile_config_proxy_fingerprint: impl Into<String>,
        live_header_fingerprint: impl Into<String>,
        admission_is_live: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        Self {
            endpoint,
            guard,
            ordering,
            attempt_factory,
            profile_config_proxy_fingerprint: profile_config_proxy_fingerprint.into(),
            live_header_fingerprint: live_header_fingerprint.into(),
            admission_is_live,
        }
    }
}

#[async_trait]
impl<R> McpStreamableHttpDestinationAuthorizer for RuntimeMcpStreamableHttpDestinationAuthorizer<R>
where
    R: WebDestinationResolver + Send + Sync,
{
    fn endpoint(&self) -> SecretString {
        self.endpoint.clone()
    }

    fn profile_config_proxy_fingerprint(&self) -> String {
        self.profile_config_proxy_fingerprint.clone()
    }

    fn live_header_fingerprint(&self) -> String {
        self.live_header_fingerprint.clone()
    }

    async fn authorize_destination(
        &self,
    ) -> Result<McpStreamableHttpAuthorizedDialPlan, McpStreamableHttpDestinationError> {
        let endpoint = Url::parse(self.endpoint.expose_secret())
            .map_err(|_| McpStreamableHttpDestinationError::DestinationRejected)?;
        // preview performs URL/logical/proxy policy checks only; direct DNS remains after barrier.
        let preview = self
            .guard
            .preview(endpoint)
            .map_err(|_| McpStreamableHttpDestinationError::DestinationRejected)?;
        let attempt = self.attempt_factory.next_attempt().await?;
        let RuntimeMcpStreamableHttpAttempt {
            authorization,
            disclosure,
            reservation,
            attempt_id,
        } = attempt;
        if authorization.safe_logical_destination != preview.safe_logical_destination()
            || authorization.safe_transport_destination != preview.safe_transport_destination()
            || disclosure.safe_logical_destination() != preview.safe_logical_destination()
            || disclosure.safe_transport_destination() != preview.safe_transport_destination()
            || authorization.profile_config_proxy_fingerprint
                != self.profile_config_proxy_fingerprint
        {
            return Err(McpStreamableHttpDestinationError::PreEgressRejected);
        }
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
            .map_err(|_| McpStreamableHttpDestinationError::DestinationRejected)?;
        match plan.route() {
            WebFetchRoute::Direct => McpStreamableHttpAuthorizedDialPlan::direct(
                self.endpoint.clone(),
                plan.safe_logical_destination(),
                plan.direct_addresses().to_vec(),
                self.profile_config_proxy_fingerprint.clone(),
                self.live_header_fingerprint.clone(),
                budget,
            )
            .map_err(|_| McpStreamableHttpDestinationError::DestinationRejected),
            WebFetchRoute::EnvironmentProxy => {
                let proxy_url = plan
                    .proxy_url()
                    .cloned()
                    .ok_or(McpStreamableHttpDestinationError::DestinationRejected)?;
                McpStreamableHttpAuthorizedDialPlan::environment_proxy(
                    self.endpoint.clone(),
                    plan.safe_logical_destination(),
                    plan.safe_transport_destination(),
                    proxy_url,
                    self.profile_config_proxy_fingerprint.clone(),
                    self.live_header_fingerprint.clone(),
                    budget,
                )
                .map_err(|_| McpStreamableHttpDestinationError::DestinationRejected)
            }
        }
    }
}

fn map_ordering_error(error: EgressOrderingError) -> McpStreamableHttpDestinationError {
    match error {
        EgressOrderingError::Budget(_) => McpStreamableHttpDestinationError::BudgetExhausted,
        _ => McpStreamableHttpDestinationError::PreEgressRejected,
    }
}

#[cfg(test)]
#[path = "tests/streamable_http_tests.rs"]
mod tests;
