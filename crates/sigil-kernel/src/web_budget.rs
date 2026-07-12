use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use thiserror::Error;
use uuid::Uuid;

const BUDGET_ID_MAX_BYTES: usize = 512;
const BUDGET_HOST_MAX_BYTES: usize = 253;

/// Hard caps shared by a complete top-level run tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebTaskTreeBudgetLimits {
    pub max_fetch_calls: u64,
    pub max_client_search_calls: u64,
    pub max_hosted_requests: u64,
    pub max_network_attempts: u64,
    pub max_wire_bytes: u64,
    pub max_decoded_bytes: u64,
    pub max_model_bytes: u64,
    pub max_concurrent_requests: u64,
    pub max_attempts_per_host: u64,
}

impl WebTaskTreeBudgetLimits {
    fn validate(self) -> Result<Self, WebBudgetError> {
        if self.max_fetch_calls == 0
            || self.max_client_search_calls == 0
            || self.max_hosted_requests == 0
            || self.max_network_attempts == 0
            || self.max_wire_bytes == 0
            || self.max_decoded_bytes == 0
            || self.max_model_bytes == 0
            || self.max_concurrent_requests == 0
            || self.max_attempts_per_host == 0
        {
            return Err(WebBudgetError::InvalidRequest(
                "all web task-tree budget limits must be non-zero".to_owned(),
            ));
        }
        Ok(self)
    }
}

/// Logical capacity consumed when request-body egress begins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebBudgetReservationKind {
    FetchCall,
    ClientSearchCall,
    HostedProviderRequest,
    TransportLifecycle,
}

/// Input for one provisional reservation after config/input/policy preflight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebBudgetReservationRequest {
    pub correlation_id: String,
    pub attempt_id: String,
    pub route_lease_id: String,
    pub route_fingerprint: String,
    pub kind: WebBudgetReservationKind,
}

/// Byte dimensions charged atomically for each received or emitted chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebBudgetByteKind {
    Wire,
    Decoded,
    Model,
}

/// Observable, secret-free budget state for tests, diagnostics and future Doctor projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebTaskTreeBudgetSnapshot {
    pub root_run_id: String,
    pub provisional_reservations: u64,
    pub logical_calls: u64,
    pub fetch_calls: u64,
    pub client_search_calls: u64,
    pub hosted_requests: u64,
    pub network_attempts: u64,
    pub wire_bytes: u64,
    pub decoded_bytes: u64,
    pub model_bytes: u64,
    pub active_concurrent_requests: u64,
    pub attempts_per_host: BTreeMap<String, u64>,
    pub exhausted: bool,
    pub cleanup_incomplete: bool,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum WebBudgetError {
    #[error("invalid web budget request: {0}")]
    InvalidRequest(String),
    #[error("web task-tree budget is exhausted for {dimension}")]
    Exhausted { dimension: &'static str },
    #[error("web budget reservation is stale or no longer active")]
    StaleReservation,
    #[error("web budget reservation does not match the current correlation or route lease")]
    ReservationMismatch,
    #[error("web budget state lock is poisoned")]
    StatePoisoned,
}

#[derive(Debug)]
struct ReservationState {
    request: WebBudgetReservationRequest,
    call_committed: bool,
    committed_attempt_ids: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct BudgetState {
    reservations: HashMap<String, ReservationState>,
    logical_calls: u64,
    fetch_calls: u64,
    client_search_calls: u64,
    hosted_requests: u64,
    network_attempts: u64,
    wire_bytes: u64,
    decoded_bytes: u64,
    model_bytes: u64,
    active_concurrent_requests: u64,
    attempts_per_host: BTreeMap<String, u64>,
    cleanup_incomplete: bool,
}

/// Unique root-owned state shared by main, planner, read/explore children and provider attempts.
///
/// Callers receive only `Arc` handles and cannot replace limits or the root id. The optional
/// cancellation hook belongs to the root construction site and fires once on hard exhaustion or
/// an unsafe early concurrency-permit drop.
pub struct WebTaskTreeBudget {
    root_run_id: String,
    limits: WebTaskTreeBudgetLimits,
    state: Mutex<BudgetState>,
    exhausted: AtomicBool,
    cancellation_hook: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl fmt::Debug for WebTaskTreeBudget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebTaskTreeBudget")
            .field("root_run_id", &self.root_run_id)
            .field("limits", &self.limits)
            .field("exhausted", &self.exhausted.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl WebTaskTreeBudget {
    pub fn new(
        root_run_id: impl Into<String>,
        limits: WebTaskTreeBudgetLimits,
        cancellation_hook: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Result<Arc<Self>, WebBudgetError> {
        let root_run_id = root_run_id.into();
        validate_budget_identity("root_run_id", &root_run_id)?;
        Ok(Arc::new(Self {
            root_run_id,
            limits: limits.validate()?,
            state: Mutex::new(BudgetState::default()),
            exhausted: AtomicBool::new(false),
            cancellation_hook,
        }))
    }

    #[must_use]
    pub fn root_run_id(&self) -> &str {
        &self.root_run_id
    }

    pub fn reserve(
        self: &Arc<Self>,
        request: WebBudgetReservationRequest,
    ) -> Result<WebBudgetReservation, WebBudgetError> {
        validate_reservation_request(&request)?;
        if self.exhausted.load(Ordering::SeqCst) {
            return Err(WebBudgetError::Exhausted {
                dimension: "task_tree",
            });
        }
        let reservation_id = format!("web-reservation-{}", Uuid::new_v4());
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        if state
            .reservations
            .values()
            .any(|existing| existing.request.correlation_id == request.correlation_id)
        {
            return Err(WebBudgetError::InvalidRequest(
                "correlation_id already has an active provisional reservation".to_owned(),
            ));
        }
        let provisional_same_kind = state
            .reservations
            .values()
            .filter(|entry| entry.request.kind == request.kind && !entry.call_committed)
            .count() as u64;
        let limited_dimension = match request.kind {
            WebBudgetReservationKind::FetchCall => (
                state.fetch_calls,
                self.limits.max_fetch_calls,
                "fetch_calls",
            ),
            WebBudgetReservationKind::ClientSearchCall => (
                state.client_search_calls,
                self.limits.max_client_search_calls,
                "client_search_calls",
            ),
            WebBudgetReservationKind::HostedProviderRequest => (
                state.hosted_requests,
                self.limits.max_hosted_requests,
                "hosted_requests",
            ),
            WebBudgetReservationKind::TransportLifecycle => (0, u64::MAX, "transport_lifecycle"),
        };
        if limited_dimension.0.saturating_add(provisional_same_kind) >= limited_dimension.1 {
            drop(state);
            self.trigger_exhaustion();
            return Err(WebBudgetError::Exhausted {
                dimension: limited_dimension.2,
            });
        }
        state.reservations.insert(
            reservation_id.clone(),
            ReservationState {
                request,
                call_committed: false,
                committed_attempt_ids: BTreeSet::new(),
            },
        );
        Ok(WebBudgetReservation {
            budget: Arc::clone(self),
            reservation_id,
            active: true,
        })
    }

    pub fn acquire_concurrency(self: &Arc<Self>) -> Result<WebConcurrencyPermit, WebBudgetError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        if state.active_concurrent_requests >= self.limits.max_concurrent_requests {
            drop(state);
            self.trigger_exhaustion();
            return Err(WebBudgetError::Exhausted {
                dimension: "concurrent_requests",
            });
        }
        state.active_concurrent_requests += 1;
        Ok(WebConcurrencyPermit {
            budget: Arc::clone(self),
            released: false,
        })
    }

    pub fn snapshot(&self) -> Result<WebTaskTreeBudgetSnapshot, WebBudgetError> {
        let state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        Ok(WebTaskTreeBudgetSnapshot {
            root_run_id: self.root_run_id.clone(),
            provisional_reservations: state
                .reservations
                .values()
                .filter(|entry| !entry.call_committed && entry.committed_attempt_ids.is_empty())
                .count() as u64,
            logical_calls: state.logical_calls,
            fetch_calls: state.fetch_calls,
            client_search_calls: state.client_search_calls,
            hosted_requests: state.hosted_requests,
            network_attempts: state.network_attempts,
            wire_bytes: state.wire_bytes,
            decoded_bytes: state.decoded_bytes,
            model_bytes: state.model_bytes,
            active_concurrent_requests: state.active_concurrent_requests,
            attempts_per_host: state.attempts_per_host.clone(),
            exhausted: self.exhausted.load(Ordering::SeqCst),
            cleanup_incomplete: state.cleanup_incomplete,
        })
    }

    fn commit_call(&self, reservation_id: &str) -> Result<(), WebBudgetError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        let kind = {
            let reservation = state
                .reservations
                .get_mut(reservation_id)
                .ok_or(WebBudgetError::StaleReservation)?;
            if reservation.call_committed {
                return Ok(());
            }
            reservation.request.kind
        };
        if kind == WebBudgetReservationKind::TransportLifecycle {
            return Err(WebBudgetError::InvalidRequest(
                "transport-only reservation cannot commit a logical or hosted call".to_owned(),
            ));
        }
        state
            .reservations
            .get_mut(reservation_id)
            .expect("reservation existence checked")
            .call_committed = true;
        match kind {
            WebBudgetReservationKind::FetchCall => {
                state.logical_calls += 1;
                state.fetch_calls += 1;
            }
            WebBudgetReservationKind::ClientSearchCall => {
                state.logical_calls += 1;
                state.client_search_calls += 1;
            }
            WebBudgetReservationKind::HostedProviderRequest => state.hosted_requests += 1,
            WebBudgetReservationKind::TransportLifecycle => unreachable!("checked above"),
        }
        Ok(())
    }

    fn commit_attempt(
        &self,
        reservation_id: &str,
        attempt_id: &str,
        safe_host: &str,
    ) -> Result<(), WebBudgetError> {
        validate_budget_identity("attempt_id", attempt_id)?;
        validate_safe_host(safe_host)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        let expected_attempt = state
            .reservations
            .get(reservation_id)
            .ok_or(WebBudgetError::StaleReservation)?
            .request
            .attempt_id
            .clone();
        let committed_attempt_ids = state
            .reservations
            .get(reservation_id)
            .expect("reservation existence checked")
            .committed_attempt_ids
            .clone();
        if committed_attempt_ids.is_empty() && expected_attempt != attempt_id {
            return Err(WebBudgetError::ReservationMismatch);
        }
        if committed_attempt_ids.contains(attempt_id) {
            return Err(WebBudgetError::InvalidRequest(
                "attempt_id is already committed for this reservation".to_owned(),
            ));
        }
        if state.network_attempts >= self.limits.max_network_attempts {
            drop(state);
            self.trigger_exhaustion();
            return Err(WebBudgetError::Exhausted {
                dimension: "network_attempts",
            });
        }
        let host_attempts = state.attempts_per_host.get(safe_host).copied().unwrap_or(0);
        if host_attempts >= self.limits.max_attempts_per_host {
            drop(state);
            self.trigger_exhaustion();
            return Err(WebBudgetError::Exhausted {
                dimension: "attempts_per_host",
            });
        }
        state.network_attempts += 1;
        *state
            .attempts_per_host
            .entry(safe_host.to_owned())
            .or_default() += 1;
        state
            .reservations
            .get_mut(reservation_id)
            .expect("reservation existence checked")
            .committed_attempt_ids
            .insert(attempt_id.to_owned());
        Ok(())
    }

    fn charge_bytes(
        &self,
        reservation_id: &str,
        kind: WebBudgetByteKind,
        bytes: u64,
    ) -> Result<(), WebBudgetError> {
        if bytes == 0 {
            return Ok(());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        if !state.reservations.contains_key(reservation_id) {
            return Err(WebBudgetError::StaleReservation);
        }
        let (current, limit, dimension) = match kind {
            WebBudgetByteKind::Wire => (state.wire_bytes, self.limits.max_wire_bytes, "wire_bytes"),
            WebBudgetByteKind::Decoded => (
                state.decoded_bytes,
                self.limits.max_decoded_bytes,
                "decoded_bytes",
            ),
            WebBudgetByteKind::Model => (
                state.model_bytes,
                self.limits.max_model_bytes,
                "model_bytes",
            ),
        };
        let Some(next) = current.checked_add(bytes) else {
            drop(state);
            self.trigger_exhaustion();
            return Err(WebBudgetError::Exhausted { dimension });
        };
        if next > limit {
            drop(state);
            self.trigger_exhaustion();
            return Err(WebBudgetError::Exhausted { dimension });
        }
        match kind {
            WebBudgetByteKind::Wire => state.wire_bytes = next,
            WebBudgetByteKind::Decoded => state.decoded_bytes = next,
            WebBudgetByteKind::Model => state.model_bytes = next,
        }
        Ok(())
    }

    fn release_reservation(&self, reservation_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.reservations.remove(reservation_id);
        }
    }

    fn release_concurrency_after_quiescence(&self) -> Result<(), WebBudgetError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        state.active_concurrent_requests = state
            .active_concurrent_requests
            .checked_sub(1)
            .ok_or_else(|| {
                WebBudgetError::InvalidRequest("concurrency permit underflow".to_owned())
            })?;
        Ok(())
    }

    fn mark_unsafe_concurrency_drop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.cleanup_incomplete = true;
        }
        self.trigger_exhaustion();
    }

    fn trigger_exhaustion(&self) {
        if !self.exhausted.swap(true, Ordering::SeqCst)
            && let Some(hook) = self.cancellation_hook.as_ref()
        {
            hook();
        }
    }
}

/// Non-cloneable provisional reservation bound to one correlation, attempt and route lease.
pub struct WebBudgetReservation {
    budget: Arc<WebTaskTreeBudget>,
    reservation_id: String,
    active: bool,
}

impl fmt::Debug for WebBudgetReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebBudgetReservation")
            .field("root_run_id", &self.budget.root_run_id)
            .field("reservation_id", &self.reservation_id)
            .field("active", &self.active)
            .finish()
    }
}

impl WebBudgetReservation {
    #[must_use]
    pub fn root_run_id(&self) -> &str {
        self.budget.root_run_id()
    }

    pub fn kind(&self) -> Result<WebBudgetReservationKind, WebBudgetError> {
        self.budget
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?
            .reservations
            .get(&self.reservation_id)
            .map(|entry| entry.request.kind)
            .ok_or(WebBudgetError::StaleReservation)
    }

    pub fn matches(
        &self,
        correlation_id: &str,
        route_lease_id: &str,
        route_fingerprint: &str,
    ) -> bool {
        self.budget
            .state
            .lock()
            .ok()
            .and_then(|state| {
                state.reservations.get(&self.reservation_id).map(|entry| {
                    entry.request.correlation_id == correlation_id
                        && entry.request.route_lease_id == route_lease_id
                        && entry.request.route_fingerprint == route_fingerprint
                })
            })
            .unwrap_or(false)
    }

    pub fn matches_route(&self, root_run_id: &str, route_fingerprint: &str) -> bool {
        self.root_run_id() == root_run_id
            && self
                .budget
                .state
                .lock()
                .ok()
                .and_then(|state| {
                    state
                        .reservations
                        .get(&self.reservation_id)
                        .map(|entry| entry.request.route_fingerprint == route_fingerprint)
                })
                .unwrap_or(false)
    }

    pub fn commit_call(&mut self) -> Result<(), WebBudgetError> {
        self.ensure_active()?;
        self.budget.commit_call(&self.reservation_id)
    }

    pub fn commit_attempt(
        &mut self,
        attempt_id: &str,
        safe_host: &str,
    ) -> Result<(), WebBudgetError> {
        self.ensure_active()?;
        self.budget
            .commit_attempt(&self.reservation_id, attempt_id, safe_host)
    }

    pub fn charge_chunk(
        &mut self,
        kind: WebBudgetByteKind,
        bytes: u64,
    ) -> Result<(), WebBudgetError> {
        self.ensure_active()?;
        self.budget.charge_bytes(&self.reservation_id, kind, bytes)
    }

    /// Explicitly refunds a reservation only while it is still fully pre-wire.
    pub fn refund_pre_wire(mut self) -> Result<(), WebBudgetError> {
        self.ensure_active()?;
        let state = self
            .budget
            .state
            .lock()
            .map_err(|_| WebBudgetError::StatePoisoned)?;
        let reservation = state
            .reservations
            .get(&self.reservation_id)
            .ok_or(WebBudgetError::StaleReservation)?;
        if reservation.call_committed || !reservation.committed_attempt_ids.is_empty() {
            return Err(WebBudgetError::InvalidRequest(
                "committed call or attempt counters are never refundable".to_owned(),
            ));
        }
        drop(state);
        self.budget.release_reservation(&self.reservation_id);
        self.active = false;
        Ok(())
    }

    fn ensure_active(&self) -> Result<(), WebBudgetError> {
        if self.active {
            Ok(())
        } else {
            Err(WebBudgetError::StaleReservation)
        }
    }
}

impl Drop for WebBudgetReservation {
    fn drop(&mut self) {
        if self.active {
            self.budget.release_reservation(&self.reservation_id);
            self.active = false;
        }
    }
}

/// RAII concurrency token that can only be released after the caller proves quiescence.
pub struct WebConcurrencyPermit {
    budget: Arc<WebTaskTreeBudget>,
    released: bool,
}

impl fmt::Debug for WebConcurrencyPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebConcurrencyPermit")
            .field("root_run_id", &self.budget.root_run_id)
            .field("released", &self.released)
            .finish()
    }
}

impl WebConcurrencyPermit {
    /// Releases capacity only after the network task/process is fully quiescent.
    pub fn release_after_quiescence(mut self) -> Result<(), WebBudgetError> {
        self.budget.release_concurrency_after_quiescence()?;
        self.released = true;
        Ok(())
    }
}

impl Drop for WebConcurrencyPermit {
    fn drop(&mut self) {
        if !self.released {
            // Conservatively retain the active count: abort/drop alone is not a quiescence proof.
            self.budget.mark_unsafe_concurrency_drop();
        }
    }
}

fn validate_reservation_request(
    request: &WebBudgetReservationRequest,
) -> Result<(), WebBudgetError> {
    validate_budget_identity("correlation_id", &request.correlation_id)?;
    validate_budget_identity("attempt_id", &request.attempt_id)?;
    validate_budget_identity("route_lease_id", &request.route_lease_id)?;
    validate_budget_identity("route_fingerprint", &request.route_fingerprint)
}

fn validate_budget_identity(field: &str, value: &str) -> Result<(), WebBudgetError> {
    if value.is_empty() || value.len() > BUDGET_ID_MAX_BYTES || value.chars().any(char::is_control)
    {
        return Err(WebBudgetError::InvalidRequest(format!(
            "{field} must be non-empty, bounded and control-free"
        )));
    }
    Ok(())
}

fn validate_safe_host(value: &str) -> Result<(), WebBudgetError> {
    if value.is_empty()
        || value.len() > BUDGET_HOST_MAX_BYTES
        || value.chars().any(char::is_control)
        || value.contains(['/', '?', '#', '@'])
    {
        return Err(WebBudgetError::InvalidRequest(
            "safe_host must be a bounded host label without URL material".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/web_budget_tests.rs"]
mod tests;
