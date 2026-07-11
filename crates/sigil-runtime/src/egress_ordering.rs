use sigil_kernel::{
    DisclosurePresentationError, EgressAuditError, EgressAuditRecorder, EgressDisclosureKind,
    HostedToolAuthorization, HostedToolOutcome, HostedToolTerminalStatus,
    McpTransportAuthorization, PreEgressDisclosure, QueryEgressOutcome, QueryEgressStarted,
    QueryEgressTerminalStatus, SharedEgressDisclosurePresenter, WebBudgetByteKind, WebBudgetError,
    WebBudgetReservation, WebBudgetReservationKind, WebFetchTransportAuthorization,
    WebSearchFailureClass, validate_disclosure_receipt,
};
use thiserror::Error;

/// Typed failure from the runtime-owned pre-egress ordering barrier.
#[derive(Debug, Error)]
pub enum EgressOrderingError {
    #[error("the active product surface has no egress disclosure presenter")]
    MissingPresenter,
    #[error("egress authorization or route lease was revoked")]
    AdmissionRevoked,
    #[error("egress authorization, disclosure, reservation and start metadata do not match")]
    BindingMismatch,
    #[error(transparent)]
    Presentation(#[from] DisclosurePresentationError),
    #[error(transparent)]
    Audit(#[from] EgressAuditError),
    #[error(transparent)]
    Budget(#[from] WebBudgetError),
}

/// Runtime barrier shared by later hosted, webfetch and Streamable HTTP adapters.
#[derive(Clone)]
pub struct EgressOrderingCoordinator {
    recorder: EgressAuditRecorder,
    presenter: Option<SharedEgressDisclosurePresenter>,
}

impl std::fmt::Debug for EgressOrderingCoordinator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EgressOrderingCoordinator")
            .field("presenter_available", &self.presenter.is_some())
            .finish_non_exhaustive()
    }
}

impl EgressOrderingCoordinator {
    #[must_use]
    pub fn new(
        recorder: EgressAuditRecorder,
        presenter: Option<SharedEgressDisclosurePresenter>,
    ) -> Self {
        Self {
            recorder,
            presenter,
        }
    }

    /// Enforces authorization -> durable authorization -> presentation -> durable disclosure.
    /// The returned non-cloneable permit is the only value a later resolver/dialer may consume.
    pub async fn authorize_transport(
        &self,
        authorization: McpTransportAuthorization,
        disclosure: PreEgressDisclosure,
        reservation: WebBudgetReservation,
        admission_is_live: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<AuthorizedTransportEgress, EgressOrderingError> {
        ensure_admission(admission_is_live)?;
        if disclosure.kind() != EgressDisclosureKind::Transport
            || disclosure.correlation_id().is_some()
            || disclosure.disclosure_id() != authorization.disclosure_id
            || disclosure.route_fingerprint() != authorization.route_fingerprint
            || disclosure.profile_config_proxy_fingerprint()
                != authorization.profile_config_proxy_fingerprint
            || disclosure.route() != authorization.route
            || disclosure.safe_logical_destination() != authorization.safe_logical_destination
            || disclosure.safe_transport_destination() != authorization.safe_transport_destination
            || !reservation
                .matches_route(&authorization.root_run_id, &authorization.route_fingerprint)
            || reservation.kind()? == WebBudgetReservationKind::HostedProviderRequest
        {
            return Err(EgressOrderingError::BindingMismatch);
        }
        self.recorder
            .append_transport_authorization(&authorization)?;
        ensure_admission(admission_is_live)?;
        let presenter = self
            .presenter
            .as_ref()
            .ok_or(EgressOrderingError::MissingPresenter)?;
        let receipt = presenter.present(disclosure.clone()).await?;
        ensure_admission(admission_is_live)?;
        let presented = validate_disclosure_receipt(&disclosure, receipt)?;
        self.recorder.append_disclosure_presented(&presented)?;
        ensure_admission(admission_is_live)?;
        Ok(AuthorizedTransportEgress {
            authorization_id: authorization.authorization_id,
            disclosure_id: authorization.disclosure_id,
            route_fingerprint: authorization.route_fingerprint,
            reservation: Some(reservation),
        })
    }

    /// Enforces the same barrier for one built-in WebFetch hop without reusing MCP semantics.
    pub async fn authorize_webfetch_transport(
        &self,
        authorization: WebFetchTransportAuthorization,
        disclosure: PreEgressDisclosure,
        reservation: WebBudgetReservation,
        admission_is_live: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<AuthorizedTransportEgress, EgressOrderingError> {
        ensure_admission(admission_is_live)?;
        if disclosure.kind() != EgressDisclosureKind::Transport
            || disclosure.correlation_id().is_some()
            || disclosure.disclosure_id() != authorization.disclosure_id
            || disclosure.route_fingerprint() != authorization.route_fingerprint
            || disclosure.profile_config_proxy_fingerprint()
                != authorization.profile_config_proxy_fingerprint
            || disclosure.route() != authorization.route
            || disclosure.safe_logical_destination() != authorization.safe_logical_destination
            || disclosure.safe_transport_destination() != authorization.safe_transport_destination
            || !reservation
                .matches_route(&authorization.root_run_id, &authorization.route_fingerprint)
            || reservation.kind()? != WebBudgetReservationKind::LogicalCall
        {
            return Err(EgressOrderingError::BindingMismatch);
        }
        self.recorder
            .append_webfetch_transport_authorization(&authorization)?;
        ensure_admission(admission_is_live)?;
        let presenter = self
            .presenter
            .as_ref()
            .ok_or(EgressOrderingError::MissingPresenter)?;
        let receipt = presenter.present(disclosure.clone()).await?;
        ensure_admission(admission_is_live)?;
        let presented = validate_disclosure_receipt(&disclosure, receipt)?;
        self.recorder.append_disclosure_presented(&presented)?;
        ensure_admission(admission_is_live)?;
        Ok(AuthorizedTransportEgress {
            authorization_id: authorization.authorization_id,
            disclosure_id: authorization.disclosure_id,
            route_fingerprint: authorization.route_fingerprint,
            reservation: Some(reservation),
        })
    }

    /// Enforces presentation -> durable disclosure -> durable query start before body bytes.
    pub async fn authorize_query(
        &self,
        disclosure: PreEgressDisclosure,
        started: QueryEgressStarted,
        reservation: WebBudgetReservation,
        admission_is_live: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<AuthorizedQueryEgress, EgressOrderingError> {
        ensure_admission(admission_is_live)?;
        if disclosure.kind() != EgressDisclosureKind::Query
            || disclosure.correlation_id() != Some(started.correlation_id.as_str())
            || disclosure.route_fingerprint() != started.route_fingerprint
            || reservation.root_run_id() != started.root_run_id
            || reservation.kind()? != WebBudgetReservationKind::LogicalCall
            || !reservation.matches(
                &started.correlation_id,
                &started.route_lease_id,
                &started.route_fingerprint,
            )
        {
            return Err(EgressOrderingError::BindingMismatch);
        }
        let presenter = self
            .presenter
            .as_ref()
            .ok_or(EgressOrderingError::MissingPresenter)?;
        let receipt = presenter.present(disclosure.clone()).await?;
        ensure_admission(admission_is_live)?;
        let presented = validate_disclosure_receipt(&disclosure, receipt)?;
        self.recorder.append_disclosure_presented(&presented)?;
        ensure_admission(admission_is_live)?;
        self.recorder.append_query_started(&started)?;
        if !admission_is_live() {
            let outcome = QueryEgressOutcome {
                record_id: format!("query-outcome-{}-cancelled", started.correlation_id),
                root_run_id: started.root_run_id.clone(),
                correlation_id: started.correlation_id.clone(),
                route_fingerprint: started.route_fingerprint.clone(),
                status: QueryEgressTerminalStatus::Cancelled,
                error_class: None,
            };
            self.recorder.append_query_outcome(&outcome)?;
            return Err(EgressOrderingError::AdmissionRevoked);
        }
        Ok(AuthorizedQueryEgress {
            recorder: self.recorder.clone(),
            started,
            reservation: Some(reservation),
        })
    }

    /// Hosted provider authorization uses the same strict durable writer before request emission.
    pub fn authorize_hosted_request(
        &self,
        authorization: &HostedToolAuthorization,
        reservation: WebBudgetReservation,
        admission_is_live: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<AuthorizedHostedEgress, EgressOrderingError> {
        ensure_admission(admission_is_live)?;
        if reservation.kind()? != WebBudgetReservationKind::HostedProviderRequest
            || !reservation.matches(
                &authorization.correlation_id,
                &authorization.route_lease_id,
                &authorization.hosted_request_fingerprint,
            )
            || reservation.root_run_id() != authorization.root_run_id
        {
            return Err(EgressOrderingError::BindingMismatch);
        }
        self.recorder.append_hosted_authorization(authorization)?;
        if !admission_is_live() {
            self.recorder.append_hosted_outcome(&HostedToolOutcome {
                record_id: format!(
                    "hosted-outcome-{}-cancelled",
                    authorization.authorization_id
                ),
                root_run_id: authorization.root_run_id.clone(),
                correlation_id: authorization.correlation_id.clone(),
                authorization_id: authorization.authorization_id.clone(),
                hosted_request_fingerprint: authorization.hosted_request_fingerprint.clone(),
                status: HostedToolTerminalStatus::Cancelled,
            })?;
            return Err(EgressOrderingError::AdmissionRevoked);
        }
        Ok(AuthorizedHostedEgress {
            recorder: self.recorder.clone(),
            authorization: authorization.clone(),
            reservation: Some(reservation),
        })
    }
}

fn ensure_admission(
    admission_is_live: &(dyn Fn() -> bool + Send + Sync),
) -> Result<(), EgressOrderingError> {
    if admission_is_live() {
        Ok(())
    } else {
        Err(EgressOrderingError::AdmissionRevoked)
    }
}

/// One-shot permit proving a transport disclosure barrier completed before DNS/dial.
#[derive(Debug)]
pub struct AuthorizedTransportEgress {
    authorization_id: String,
    disclosure_id: String,
    route_fingerprint: String,
    reservation: Option<WebBudgetReservation>,
}

impl AuthorizedTransportEgress {
    #[must_use]
    pub fn authorization_id(&self) -> &str {
        &self.authorization_id
    }

    #[must_use]
    pub fn disclosure_id(&self) -> &str {
        &self.disclosure_id
    }

    #[must_use]
    pub fn route_fingerprint(&self) -> &str {
        &self.route_fingerprint
    }

    /// Commits one connect/reconnect attempt immediately before DNS/dial.
    pub fn begin_attempt(
        mut self,
        attempt_id: &str,
        safe_host: &str,
    ) -> Result<WebBudgetReservation, EgressOrderingError> {
        let mut reservation = self
            .reservation
            .take()
            .ok_or(WebBudgetError::StaleReservation)?;
        reservation.commit_attempt(attempt_id, safe_host)?;
        Ok(reservation)
    }
}

/// Active hosted provider request capability with an explicit unique terminal append.
pub struct AuthorizedHostedEgress {
    recorder: EgressAuditRecorder,
    authorization: HostedToolAuthorization,
    reservation: Option<WebBudgetReservation>,
}

impl std::fmt::Debug for AuthorizedHostedEgress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedHostedEgress")
            .field("authorization_id", &self.authorization.authorization_id)
            .field("correlation_id", &self.authorization.correlation_id)
            .finish_non_exhaustive()
    }
}

impl AuthorizedHostedEgress {
    /// Commits the hosted-request count at the provider request first-byte boundary.
    pub fn begin_request(mut self) -> Result<ActiveHostedEgress, EgressOrderingError> {
        let mut reservation = self
            .reservation
            .take()
            .ok_or(WebBudgetError::StaleReservation)?;
        if let Err(error) = reservation.commit_call() {
            self.recorder.append_hosted_outcome(&hosted_outcome(
                &self.authorization,
                HostedToolTerminalStatus::RequestFailed,
            ))?;
            return Err(error.into());
        }
        Ok(ActiveHostedEgress {
            recorder: self.recorder,
            authorization: self.authorization,
            reservation: Some(reservation),
            terminal: false,
        })
    }
}

/// Active post-send hosted request with model-byte charging and one terminal outcome.
pub struct ActiveHostedEgress {
    recorder: EgressAuditRecorder,
    authorization: HostedToolAuthorization,
    reservation: Option<WebBudgetReservation>,
    terminal: bool,
}

impl std::fmt::Debug for ActiveHostedEgress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveHostedEgress")
            .field("authorization_id", &self.authorization.authorization_id)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl ActiveHostedEgress {
    pub fn charge_model_chunk(&mut self, bytes: u64) -> Result<(), EgressOrderingError> {
        let result = self
            .reservation
            .as_mut()
            .ok_or(WebBudgetError::StaleReservation)?
            .charge_chunk(WebBudgetByteKind::Model, bytes);
        match result {
            Ok(()) => Ok(()),
            Err(error @ WebBudgetError::Exhausted { .. }) => {
                self.finish_request_failed()?;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn finish(mut self, status: HostedToolTerminalStatus) -> Result<(), EgressOrderingError> {
        self.recorder
            .append_hosted_outcome(&hosted_outcome(&self.authorization, status))?;
        self.terminal = true;
        self.reservation.take();
        Ok(())
    }

    fn finish_request_failed(&mut self) -> Result<(), EgressOrderingError> {
        if self.terminal {
            return Ok(());
        }
        self.recorder.append_hosted_outcome(&hosted_outcome(
            &self.authorization,
            HostedToolTerminalStatus::RequestFailed,
        ))?;
        self.terminal = true;
        self.reservation.take();
        Ok(())
    }
}

fn hosted_outcome(
    authorization: &HostedToolAuthorization,
    status: HostedToolTerminalStatus,
) -> HostedToolOutcome {
    HostedToolOutcome {
        record_id: format!(
            "hosted-outcome-{}-{}",
            authorization.authorization_id,
            hosted_terminal_label(status)
        ),
        root_run_id: authorization.root_run_id.clone(),
        correlation_id: authorization.correlation_id.clone(),
        authorization_id: authorization.authorization_id.clone(),
        hosted_request_fingerprint: authorization.hosted_request_fingerprint.clone(),
        status,
    }
}

/// One-shot proof that a query disclosure and durable start completed before body bytes.
pub struct AuthorizedQueryEgress {
    recorder: EgressAuditRecorder,
    started: QueryEgressStarted,
    reservation: Option<WebBudgetReservation>,
}

impl std::fmt::Debug for AuthorizedQueryEgress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthorizedQueryEgress")
            .field("correlation_id", &self.started.correlation_id)
            .field("route_fingerprint", &self.started.route_fingerprint)
            .finish_non_exhaustive()
    }
}

impl AuthorizedQueryEgress {
    /// Appends the unique terminal when a failure happens after durable start but before body poll.
    pub fn finish_without_body(
        mut self,
        status: QueryEgressTerminalStatus,
        error_class: Option<WebSearchFailureClass>,
    ) -> Result<(), EgressOrderingError> {
        self.recorder.append_query_outcome(&QueryEgressOutcome {
            record_id: format!(
                "query-outcome-{}-{}",
                self.started.correlation_id,
                terminal_label(status)
            ),
            root_run_id: self.started.root_run_id.clone(),
            correlation_id: self.started.correlation_id.clone(),
            route_fingerprint: self.started.route_fingerprint.clone(),
            status,
            error_class,
        })?;
        self.reservation.take();
        Ok(())
    }

    /// Commits the logical/hosted call at the request-body first-byte boundary.
    pub fn begin_body(mut self) -> Result<ActiveQueryEgress, EgressOrderingError> {
        let mut reservation = self
            .reservation
            .take()
            .ok_or(WebBudgetError::StaleReservation)?;
        if let Err(error) = reservation.commit_call() {
            let outcome = QueryEgressOutcome {
                record_id: format!(
                    "query-outcome-{}-budget-exhausted",
                    self.started.correlation_id
                ),
                root_run_id: self.started.root_run_id.clone(),
                correlation_id: self.started.correlation_id.clone(),
                route_fingerprint: self.started.route_fingerprint.clone(),
                status: QueryEgressTerminalStatus::Failed,
                error_class: Some(WebSearchFailureClass::BudgetExhausted),
            };
            self.recorder.append_query_outcome(&outcome)?;
            return Err(EgressOrderingError::Budget(error));
        }
        Ok(ActiveQueryEgress {
            recorder: self.recorder.clone(),
            started: self.started.clone(),
            reservation: Some(reservation),
            terminal: false,
        })
    }
}

/// Active post-start query state. It owns budget charging and the unique terminal append.
pub struct ActiveQueryEgress {
    recorder: EgressAuditRecorder,
    started: QueryEgressStarted,
    reservation: Option<WebBudgetReservation>,
    terminal: bool,
}

impl std::fmt::Debug for ActiveQueryEgress {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ActiveQueryEgress")
            .field("correlation_id", &self.started.correlation_id)
            .field("terminal", &self.terminal)
            .finish_non_exhaustive()
    }
}

impl ActiveQueryEgress {
    pub fn commit_attempt(
        &mut self,
        attempt_id: &str,
        safe_host: &str,
    ) -> Result<(), EgressOrderingError> {
        match self
            .reservation_mut()?
            .commit_attempt(attempt_id, safe_host)
        {
            Ok(()) => Ok(()),
            Err(error @ WebBudgetError::Exhausted { .. }) => {
                self.finish_budget_exhausted()?;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn charge_chunk(
        &mut self,
        kind: WebBudgetByteKind,
        bytes: u64,
    ) -> Result<(), EgressOrderingError> {
        match self.reservation_mut()?.charge_chunk(kind, bytes) {
            Ok(()) => Ok(()),
            Err(error @ WebBudgetError::Exhausted { .. }) => {
                self.finish_budget_exhausted()?;
                Err(error.into())
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn finish(
        mut self,
        status: QueryEgressTerminalStatus,
        error_class: Option<WebSearchFailureClass>,
    ) -> Result<(), EgressOrderingError> {
        if self.terminal {
            return Ok(());
        }
        let outcome = QueryEgressOutcome {
            record_id: format!(
                "query-outcome-{}-{}",
                self.started.correlation_id,
                terminal_label(status)
            ),
            root_run_id: self.started.root_run_id.clone(),
            correlation_id: self.started.correlation_id.clone(),
            route_fingerprint: self.started.route_fingerprint.clone(),
            status,
            error_class,
        };
        self.recorder.append_query_outcome(&outcome)?;
        self.terminal = true;
        self.reservation.take();
        Ok(())
    }

    fn reservation_mut(&mut self) -> Result<&mut WebBudgetReservation, EgressOrderingError> {
        self.reservation
            .as_mut()
            .ok_or(WebBudgetError::StaleReservation.into())
    }

    fn finish_budget_exhausted(&mut self) -> Result<(), EgressOrderingError> {
        if self.terminal {
            return Ok(());
        }
        self.recorder.append_query_outcome(&QueryEgressOutcome {
            record_id: format!(
                "query-outcome-{}-budget-exhausted",
                self.started.correlation_id
            ),
            root_run_id: self.started.root_run_id.clone(),
            correlation_id: self.started.correlation_id.clone(),
            route_fingerprint: self.started.route_fingerprint.clone(),
            status: QueryEgressTerminalStatus::Failed,
            error_class: Some(WebSearchFailureClass::BudgetExhausted),
        })?;
        self.terminal = true;
        self.reservation.take();
        Ok(())
    }
}

fn hosted_terminal_label(status: HostedToolTerminalStatus) -> &'static str {
    match status {
        HostedToolTerminalStatus::Observed => "observed",
        HostedToolTerminalStatus::NotUsed => "not-used",
        HostedToolTerminalStatus::RequestFailed => "request-failed",
        HostedToolTerminalStatus::Cancelled => "cancelled",
        HostedToolTerminalStatus::Interrupted => "interrupted",
    }
}

fn terminal_label(status: QueryEgressTerminalStatus) -> &'static str {
    match status {
        QueryEgressTerminalStatus::Completed => "completed",
        QueryEgressTerminalStatus::Failed => "failed",
        QueryEgressTerminalStatus::RateLimited => "rate-limited",
        QueryEgressTerminalStatus::Cancelled => "cancelled",
        QueryEgressTerminalStatus::Interrupted => "interrupted",
    }
}

#[cfg(test)]
#[path = "tests/egress_ordering_tests.rs"]
mod tests;
