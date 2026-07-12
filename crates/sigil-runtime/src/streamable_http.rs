use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use sigil_kernel::{
    EgressBindingOrigin, EgressDataCategory, EgressDisclosureKind, EgressNetworkRoute,
    McpTransportAuthorization, PreEgressDisclosure, SecretString, WebBudgetReservation,
    WebBudgetReservationKind, WebBudgetReservationRequest, WebTaskTreeBudget,
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

/// Production attempt factory that creates a fresh durable identity and provisional shared-budget
/// reservation for every Streamable HTTP message. It contains only safe destination metadata;
/// resolved credentials remain owned by `PreparedMcpStreamableHttpHeaders`.
pub struct RuntimeMcpTransportAttemptFactory {
    budget_binding: Mutex<RuntimeMcpBudgetBinding>,
    binding_origin: EgressBindingOrigin,
    disclosure_id: String,
    surface: String,
    display_name: String,
    route_fingerprint: String,
    profile_config_proxy_fingerprint: String,
    safe_logical_destination: String,
    safe_transport_destination: String,
    route: EgressNetworkRoute,
    data_categories: Vec<EgressDataCategory>,
}

struct RuntimeMcpBudgetBinding {
    budget: Arc<WebTaskTreeBudget>,
    root_run_id: String,
}

impl RuntimeMcpTransportAttemptFactory {
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new(
        budget: Arc<WebTaskTreeBudget>,
        root_run_id: impl Into<String>,
        binding_origin: EgressBindingOrigin,
        disclosure_id: impl Into<String>,
        surface: impl Into<String>,
        display_name: impl Into<String>,
        route_fingerprint: impl Into<String>,
        profile_config_proxy_fingerprint: impl Into<String>,
        safe_logical_destination: impl Into<String>,
        safe_transport_destination: impl Into<String>,
        route: EgressNetworkRoute,
        data_categories: Vec<EgressDataCategory>,
    ) -> Self {
        Self {
            budget_binding: Mutex::new(RuntimeMcpBudgetBinding {
                budget,
                root_run_id: root_run_id.into(),
            }),
            binding_origin,
            disclosure_id: disclosure_id.into(),
            surface: surface.into(),
            display_name: display_name.into(),
            route_fingerprint: route_fingerprint.into(),
            profile_config_proxy_fingerprint: profile_config_proxy_fingerprint.into(),
            safe_logical_destination: safe_logical_destination.into(),
            safe_transport_destination: safe_transport_destination.into(),
            route,
            data_categories,
        }
    }

    /// Rebinds a long-lived remote client to the current top-level run budget.
    ///
    /// Callers must hold their client-wide execution lock until the HTTP operation completes so
    /// another run cannot replace this binding between request messages.
    pub fn rebind_budget(
        &self,
        budget: Arc<WebTaskTreeBudget>,
    ) -> Result<(), McpStreamableHttpDestinationError> {
        let root_run_id = budget.root_run_id().to_owned();
        let mut binding = self
            .budget_binding
            .lock()
            .map_err(|_| McpStreamableHttpDestinationError::BudgetExhausted)?;
        *binding = RuntimeMcpBudgetBinding {
            budget,
            root_run_id,
        };
        Ok(())
    }
}

#[async_trait]
impl RuntimeMcpStreamableHttpAttemptFactory for RuntimeMcpTransportAttemptFactory {
    async fn next_attempt(
        &self,
    ) -> Result<RuntimeMcpStreamableHttpAttempt, McpStreamableHttpDestinationError> {
        let unique = uuid::Uuid::new_v4();
        let authorization_id = format!("mcp-transport-auth-{unique}");
        let attempt_id = format!("mcp-transport-attempt-{unique}");
        let disclosure_id = format!("{}-{unique}", self.disclosure_id);
        let (budget, root_run_id) = {
            let binding = self
                .budget_binding
                .lock()
                .map_err(|_| McpStreamableHttpDestinationError::BudgetExhausted)?;
            (Arc::clone(&binding.budget), binding.root_run_id.clone())
        };
        let reservation = budget
            .reserve(WebBudgetReservationRequest {
                correlation_id: authorization_id.clone(),
                attempt_id: attempt_id.clone(),
                route_lease_id: authorization_id.clone(),
                route_fingerprint: self.route_fingerprint.clone(),
                kind: WebBudgetReservationKind::TransportLifecycle,
            })
            .map_err(|_| McpStreamableHttpDestinationError::BudgetExhausted)?;
        let authorization = McpTransportAuthorization {
            record_id: format!("mcp-transport-authorization-{unique}"),
            root_run_id,
            authorization_id,
            disclosure_id: disclosure_id.clone(),
            binding_origin: self.binding_origin,
            route_fingerprint: self.route_fingerprint.clone(),
            profile_config_proxy_fingerprint: self.profile_config_proxy_fingerprint.clone(),
            route: self.route,
            safe_logical_destination: self.safe_logical_destination.clone(),
            safe_transport_destination: self.safe_transport_destination.clone(),
        };
        let disclosure = PreEgressDisclosure::new(
            EgressDisclosureKind::Transport,
            None,
            disclosure_id,
            self.surface.clone(),
            self.display_name.clone(),
            self.route_fingerprint.clone(),
            self.profile_config_proxy_fingerprint.clone(),
            self.safe_logical_destination.clone(),
            self.safe_transport_destination.clone(),
            self.route,
            self.data_categories.clone(),
        )
        .map_err(|_| McpStreamableHttpDestinationError::PreEgressRejected)?;
        Ok(RuntimeMcpStreamableHttpAttempt::new(
            authorization,
            disclosure,
            reservation,
            attempt_id,
        ))
    }
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
    transport_fingerprint: Option<String>,
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
            transport_fingerprint: None,
            live_header_fingerprint: live_header_fingerprint.into(),
            admission_is_live,
        }
    }

    /// Overrides the transport-neutral static identity while keeping live header/proxy state in
    /// the stronger profile binding.
    #[must_use]
    pub fn with_transport_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.transport_fingerprint = Some(fingerprint.into());
        self
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

    fn transport_fingerprint(&self) -> String {
        self.transport_fingerprint
            .clone()
            .unwrap_or_else(|| self.profile_config_proxy_fingerprint.clone())
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
