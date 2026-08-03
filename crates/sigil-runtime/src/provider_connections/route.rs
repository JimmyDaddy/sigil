use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{Arc, Mutex},
};

use anyhow::Result;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sigil_kernel::{
    ConnectionId, JsonlSessionStore, ModelRef, ResolvedModelRoute, RootConfig,
    RouteEgressTrustBinding, Session,
};

use super::{
    ConfigMode, ProviderConnectionConfig, ProviderFamily, ProviderProtocol,
    connection_semantic_fingerprint, load_provider_connections,
};

#[derive(Debug, thiserror::Error)]
pub enum ResolvedRouteError {
    #[error("model_route_not_configured")]
    NotConfigured,
    #[error("connection_not_found")]
    ConnectionNotFound,
    #[error("connection_config_invalid")]
    ConnectionConfigInvalid,
    #[error("session_route_drift")]
    SemanticDrift,
}

/// Immutable, secret-free view used to plan and apply one route resume decision.
///
/// The snapshot owns the exact root configuration value from which its admitted route records
/// were resolved. Replacing the on-disk configuration cannot change this value; callers must
/// explicitly construct a new snapshot after a save or reload.
#[derive(Clone)]
pub struct ResolvedRouteConfigSnapshot {
    root_config: RootConfig,
    mode: ConfigMode,
    default_model: Option<ModelRef>,
    connections: BTreeMap<ConnectionId, ResolvedRouteSnapshotConnection>,
    invalid_connection_ids: BTreeSet<String>,
    binding: String,
}

impl fmt::Debug for ResolvedRouteConfigSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedRouteConfigSnapshot")
            .field("mode", &self.mode)
            .field("default_model", &self.default_model)
            .field(
                "connection_ids",
                &self.connections.keys().collect::<Vec<_>>(),
            )
            .field("invalid_connection_ids", &self.invalid_connection_ids)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedRouteSnapshotConnection {
    provider_name: String,
    provider_family: String,
    protocol: String,
    semantic_fingerprint: String,
    egress_trust_binding: RouteEgressTrustBinding,
}

impl ResolvedRouteConfigSnapshot {
    /// Resolves all admitted connection records exactly once from the supplied configuration.
    #[must_use]
    pub fn from_root_config(root_config: &RootConfig) -> Self {
        let loaded = load_provider_connections(root_config);
        let connections = loaded
            .connections
            .iter()
            .map(|(connection_id, loaded_connection)| {
                let connection = &loaded_connection.config;
                (
                    connection_id.clone(),
                    ResolvedRouteSnapshotConnection {
                        provider_name: runtime_provider_name(connection).to_owned(),
                        provider_family: connection.provider.as_str().to_owned(),
                        protocol: connection.protocol.as_str().to_owned(),
                        semantic_fingerprint: connection_semantic_fingerprint(connection),
                        egress_trust_binding: connection_egress_trust_binding(connection),
                    },
                )
            })
            .collect();
        let invalid_connection_ids = loaded
            .issues
            .iter()
            .filter(|issue| issue.code == "invalid_connection")
            .filter_map(|issue| issue.connection_id.clone())
            .collect();
        let binding =
            route_snapshot_binding(loaded.mode, loaded.default_model.as_ref(), &connections);
        Self {
            root_config: root_config.clone(),
            mode: loaded.mode,
            default_model: loaded.default_model,
            connections,
            invalid_connection_ids,
            binding,
        }
    }

    /// Returns the immutable configuration captured with this resolution view.
    #[must_use]
    pub fn root_config(&self) -> &RootConfig {
        &self.root_config
    }

    /// Returns the default model admitted by this snapshot, when configured.
    #[must_use]
    pub fn default_model(&self) -> Option<&ModelRef> {
        self.default_model.as_ref()
    }

    /// Returns the opaque identity of this immutable, secret-free route snapshot.
    #[must_use]
    pub fn binding(&self) -> &str {
        &self.binding
    }

    /// Binds one recovery choice to this snapshot and the exact durable route boundary.
    #[must_use]
    pub fn recovery_binding(
        &self,
        session_scope_id: &str,
        source_route: &ResolvedModelRoute,
        session_frontier_binding: &str,
        route_authority_generation_binding: &str,
    ) -> String {
        route_recovery_binding(
            session_scope_id,
            source_route,
            session_frontier_binding,
            route_authority_generation_binding,
            self,
        )
    }

    /// Returns the admitted runtime provider, exact route, and trust proof for one model ref.
    pub fn resolved_route(
        &self,
        model_ref: &ModelRef,
    ) -> Option<(String, ResolvedModelRoute, RouteEgressTrustBinding)> {
        self.resolve_model_ref(model_ref).ok().map(|resolved| {
            (
                resolved.provider_name,
                resolved.route,
                resolved.egress_trust_binding,
            )
        })
    }

    fn resolve_model_ref(
        &self,
        model_ref: &ModelRef,
    ) -> std::result::Result<ResolvedSnapshotRoute, SnapshotRouteUnavailable> {
        if self.mode != ConfigMode::V2 {
            return Err(SnapshotRouteUnavailable::Setup(
                ModelRouteSetupReason::ConfigurationInvalid,
            ));
        }
        let Some(connection) = self.connections.get(&model_ref.connection_id) else {
            return if self
                .invalid_connection_ids
                .contains(model_ref.connection_id.as_str())
            {
                Err(SnapshotRouteUnavailable::Connection(
                    SessionRouteUnavailableReason::ConnectionConfigInvalid,
                ))
            } else {
                Err(SnapshotRouteUnavailable::Connection(
                    SessionRouteUnavailableReason::ConnectionNotFound,
                ))
            };
        };
        let route = ResolvedModelRoute::new(
            model_ref.clone(),
            connection.provider_family.clone(),
            connection.protocol.clone(),
            connection.semantic_fingerprint.clone(),
        )
        .expect("snapshot only stores admitted route material");
        Ok(ResolvedSnapshotRoute {
            provider_name: connection.provider_name.clone(),
            route,
            egress_trust_binding: connection.egress_trust_binding.clone(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedSnapshotRoute {
    provider_name: String,
    route: ResolvedModelRoute,
    egress_trust_binding: RouteEgressTrustBinding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotRouteUnavailable {
    Connection(SessionRouteUnavailableReason),
    Setup(ModelRouteSetupReason),
}

/// Persisted route facts required by the pure portable-resume planner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRouteResumeInput {
    pub route: ResolvedModelRoute,
    pub egress_trust_binding: Option<RouteEgressTrustBinding>,
}

/// Product-level disposition for opening and continuing a durable session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionRouteResumePlan {
    Exact {
        provider_name: String,
        route: ResolvedModelRoute,
    },
    RebindCurrentModel {
        provider_name: String,
        source_route: ResolvedModelRoute,
        target_route: ResolvedModelRoute,
        egress_trust_binding: RouteEgressTrustBinding,
        reason: SessionRouteRebindReason,
    },
    NeedsConfirmation {
        provider_name: String,
        source_route: ResolvedModelRoute,
        target_route: ResolvedModelRoute,
        target_egress_trust_binding: RouteEgressTrustBinding,
        reason: SessionRouteConfirmationReason,
    },
    NeedsReplacement {
        source_route: ResolvedModelRoute,
        reason: SessionRouteUnavailableReason,
    },
    NeedsSetup {
        reason: ModelRouteSetupReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRouteRebindReason {
    ConnectionSemanticsChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRouteConfirmationReason {
    EgressTrustChanged,
    EgressTrustUnproven,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRouteUnavailableReason {
    ConnectionNotFound,
    ConnectionConfigInvalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRouteSetupReason {
    ConfigurationInvalid,
    RouteNotConfigured,
}

/// Plans portable session resume without filesystem writes, credential reads, or network access.
#[must_use]
pub fn plan_session_route_resume(
    config_snapshot: &ResolvedRouteConfigSnapshot,
    persisted: &SessionRouteResumeInput,
) -> SessionRouteResumePlan {
    let current = match config_snapshot.resolve_model_ref(&persisted.route.model_ref) {
        Ok(current) => current,
        Err(SnapshotRouteUnavailable::Connection(reason)) => {
            return SessionRouteResumePlan::NeedsReplacement {
                source_route: persisted.route.clone(),
                reason,
            };
        }
        Err(SnapshotRouteUnavailable::Setup(reason)) => {
            return SessionRouteResumePlan::NeedsSetup { reason };
        }
    };

    if current.route == persisted.route {
        return SessionRouteResumePlan::Exact {
            provider_name: current.provider_name,
            route: current.route,
        };
    }

    let Some(source_binding) = persisted.egress_trust_binding.as_ref() else {
        return SessionRouteResumePlan::NeedsConfirmation {
            provider_name: current.provider_name,
            source_route: persisted.route.clone(),
            target_route: current.route,
            target_egress_trust_binding: current.egress_trust_binding,
            reason: SessionRouteConfirmationReason::EgressTrustUnproven,
        };
    };
    if source_binding != &current.egress_trust_binding {
        return SessionRouteResumePlan::NeedsConfirmation {
            provider_name: current.provider_name,
            source_route: persisted.route.clone(),
            target_route: current.route,
            target_egress_trust_binding: current.egress_trust_binding,
            reason: SessionRouteConfirmationReason::EgressTrustChanged,
        };
    }

    SessionRouteResumePlan::RebindCurrentModel {
        provider_name: current.provider_name,
        source_route: persisted.route.clone(),
        target_route: current.route,
        egress_trust_binding: current.egress_trust_binding,
        reason: SessionRouteRebindReason::ConnectionSemanticsChanged,
    }
}

/// Session-scoped authority coordinating provider execution owners and route transitions.
#[derive(Debug, Clone)]
pub struct SessionRouteMutationAuthority {
    session_scope_id: String,
    authority_id: String,
    state: Arc<Mutex<SessionRouteAuthorityState>>,
}

#[derive(Debug, Default)]
struct SessionRouteAuthorityState {
    generation: u64,
    active_owner_count: u32,
    transition_generation: Option<u64>,
}

impl SessionRouteMutationAuthority {
    /// Creates one authority for the exact durable session scope managed by a controller.
    #[must_use]
    pub fn new(session_scope_id: impl Into<String>) -> Self {
        Self {
            session_scope_id: session_scope_id.into(),
            authority_id: uuid::Uuid::new_v4().to_string(),
            state: Arc::new(Mutex::new(SessionRouteAuthorityState::default())),
        }
    }

    /// Returns the exact durable session scope fenced by this authority.
    #[must_use]
    pub fn session_scope_id(&self) -> &str {
        &self.session_scope_id
    }

    /// Acquires one foreground or background provider owner for this session.
    pub fn acquire_execution_owner(
        &self,
    ) -> Result<SessionRouteExecutionOwner, SessionRouteAuthorityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionRouteAuthorityError::Poisoned)?;
        if state.transition_generation.is_some() {
            return Err(SessionRouteAuthorityError::TransitionInProgress);
        }
        state.active_owner_count = state
            .active_owner_count
            .checked_add(1)
            .ok_or(SessionRouteAuthorityError::OwnerCountOverflow)?;
        Ok(SessionRouteExecutionOwner {
            state: Arc::clone(&self.state),
            released: false,
        })
    }

    /// Issues a generation-bound one-shot permit only when all provider owners are terminal.
    pub fn issue_quiescence_permit(
        &self,
    ) -> Result<SessionRouteMutationPermit, SessionRouteAuthorityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| SessionRouteAuthorityError::Poisoned)?;
        if state.active_owner_count != 0 {
            return Err(SessionRouteAuthorityError::ActiveOwners);
        }
        if state.transition_generation.is_some() {
            return Err(SessionRouteAuthorityError::TransitionInProgress);
        }
        state.generation = state.generation.saturating_add(1);
        let generation = state.generation;
        state.transition_generation = Some(generation);
        Ok(SessionRouteMutationPermit {
            session_scope_id: self.session_scope_id.clone(),
            authority_id: self.authority_id.clone(),
            generation,
            authority: Arc::clone(&self.state),
            consumed: false,
        })
    }
}

/// Live provider execution ownership. Dropping it proves that owner has been joined or reaped.
#[derive(Debug)]
pub struct SessionRouteExecutionOwner {
    state: Arc<Mutex<SessionRouteAuthorityState>>,
    released: bool,
}

impl SessionRouteExecutionOwner {
    /// Explicitly releases the execution owner after its provider task is terminal.
    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        if let Ok(mut state) = self.state.lock() {
            state.active_owner_count = state.active_owner_count.saturating_sub(1);
        }
        self.released = true;
    }
}

impl Drop for SessionRouteExecutionOwner {
    fn drop(&mut self) {
        self.release_inner();
    }
}

/// Opaque one-shot proof that one session has no live provider execution owner.
#[derive(Debug)]
pub struct SessionRouteMutationPermit {
    session_scope_id: String,
    authority_id: String,
    generation: u64,
    authority: Arc<Mutex<SessionRouteAuthorityState>>,
    consumed: bool,
}

impl SessionRouteMutationPermit {
    fn enter(
        mut self,
        session_scope_id: &str,
    ) -> Result<SessionRouteMutationGuard, SessionRouteResumeError> {
        if self.session_scope_id != session_scope_id {
            return Err(SessionRouteResumeError::PermitScopeMismatch);
        }
        let state = self
            .authority
            .lock()
            .map_err(|_| SessionRouteResumeError::AuthorityStale)?;
        if self.authority_id.is_empty()
            || state.transition_generation != Some(self.generation)
            || state.active_owner_count != 0
        {
            return Err(SessionRouteResumeError::AuthorityStale);
        }
        self.consumed = true;
        drop(state);
        Ok(SessionRouteMutationGuard {
            authority: Arc::clone(&self.authority),
            generation: self.generation,
        })
    }

    fn cancel(&mut self) {
        if self.consumed {
            return;
        }
        if let Ok(mut state) = self.authority.lock()
            && state.transition_generation == Some(self.generation)
        {
            state.transition_generation = None;
        }
        self.consumed = true;
    }
}

impl Drop for SessionRouteMutationPermit {
    fn drop(&mut self) {
        self.cancel();
    }
}

#[derive(Debug)]
struct SessionRouteMutationGuard {
    authority: Arc<Mutex<SessionRouteAuthorityState>>,
    generation: u64,
}

impl Drop for SessionRouteMutationGuard {
    fn drop(&mut self) {
        if let Ok(mut state) = self.authority.lock()
            && state.transition_generation == Some(self.generation)
        {
            state.transition_generation = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SessionRouteAuthorityError {
    #[error("session_route_active_owners")]
    ActiveOwners,
    #[error("session_route_transition_in_progress")]
    TransitionInProgress,
    #[error("session_route_authority_poisoned")]
    Poisoned,
    #[error("session_route_owner_count_overflow")]
    OwnerCountOverflow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRouteResumeStatus {
    Applied,
    AlreadyApplied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRouteResumeOutcome {
    pub status: SessionRouteResumeStatus,
    pub private_state_reset: bool,
}

/// Bounded, provider-neutral receipt for the route installed by one session open/run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRouteTransitionKind {
    Exact,
    Rebound,
    ExplicitlyConfirmed,
}

/// Public-safe route transition facts. No endpoint, credential, fingerprint, or path is exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRouteTransitionView {
    pub kind: SessionRouteTransitionKind,
    pub connection_id: Option<String>,
    pub model_id: Option<String>,
    pub remote_context_reset: bool,
}

/// Attachment-aware load result including its machine-readable route receipt.
#[derive(Debug)]
pub struct SessionRouteLoadOutcome {
    pub session: Session,
    pub transition: SessionRouteTransitionView,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionRouteResumeError {
    #[error("session_route_plan_not_applicable")]
    PlanNotApplicable,
    #[error("session_route_permit_scope_mismatch")]
    PermitScopeMismatch,
    #[error("session_route_authority_stale")]
    AuthorityStale,
    #[error("session_route_snapshot_stale")]
    SnapshotStale,
    #[error("session_route_source_stale")]
    SessionRouteStale,
    #[error("session_route_rebind_commit_failed")]
    Commit(#[source] anyhow::Error),
}

/// A durable session plus its pure route decision from one immutable config snapshot.
#[derive(Debug)]
pub struct InspectedSessionRouteResume {
    pub session: Session,
    pub config_snapshot: ResolvedRouteConfigSnapshot,
    pub plan: SessionRouteResumePlan,
    pub recovery_binding: String,
}

/// Typed product-level failure to make a durable session ready for provider execution.
#[derive(Debug, thiserror::Error)]
pub enum SessionRouteLoadError {
    #[error("session_route_confirmation_required")]
    ConfirmationRequired {
        reason: SessionRouteConfirmationReason,
        recovery_binding: String,
    },
    #[error("session_route_selection_required")]
    SelectionRequired {
        reason: SessionRouteUnavailableReason,
        recovery_binding: String,
    },
    #[error("model_route_not_configured")]
    SetupRequired {
        reason: ModelRouteSetupReason,
        recovery_binding: String,
    },
    #[error("session_writer_busy")]
    WriterBusy { recovery_binding: String },
    #[error("session_route_unavailable")]
    Unavailable(#[source] anyhow::Error),
}

impl SessionRouteLoadError {
    #[must_use]
    pub fn recovery_binding(&self) -> Option<&str> {
        match self {
            Self::ConfirmationRequired {
                recovery_binding, ..
            }
            | Self::SelectionRequired {
                recovery_binding, ..
            }
            | Self::SetupRequired {
                recovery_binding, ..
            }
            | Self::WriterBusy { recovery_binding } => Some(recovery_binding),
            Self::Unavailable(_) => None,
        }
    }
}

/// Applies an automatic portable rebind after revalidating snapshot, authority, and session state.
pub fn apply_session_route_resume_plan(
    config_snapshot: &ResolvedRouteConfigSnapshot,
    session: &mut Session,
    plan: SessionRouteResumePlan,
    quiescence: SessionRouteMutationPermit,
) -> Result<SessionRouteResumeOutcome, SessionRouteResumeError> {
    let _mutation_guard = quiescence.enter(session.session_scope_id())?;
    let SessionRouteResumePlan::RebindCurrentModel {
        provider_name,
        source_route,
        target_route,
        egress_trust_binding,
        ..
    } = plan
    else {
        return Err(SessionRouteResumeError::PlanNotApplicable);
    };
    let current = config_snapshot
        .resolve_model_ref(&target_route.model_ref)
        .map_err(|_| SessionRouteResumeError::SnapshotStale)?;
    if current.provider_name != provider_name
        || current.route != target_route
        || current.egress_trust_binding != egress_trust_binding
    {
        return Err(SessionRouteResumeError::SnapshotStale);
    }

    if session.resolved_model_route() == Some(&source_route) {
        session
            .commit_route_rebind(
                provider_name,
                &source_route,
                target_route,
                egress_trust_binding,
            )
            .map_err(SessionRouteResumeError::Commit)?;
        return Ok(SessionRouteResumeOutcome {
            status: SessionRouteResumeStatus::Applied,
            private_state_reset: true,
        });
    }
    if session.resolved_model_route() == Some(&target_route)
        && session.route_rebind_matches(&source_route, &target_route, &egress_trust_binding)
    {
        return Ok(SessionRouteResumeOutcome {
            status: SessionRouteResumeStatus::AlreadyApplied,
            private_state_reset: true,
        });
    }
    Err(SessionRouteResumeError::SessionRouteStale)
}

/// Applies an exact-bound user confirmation for a changed or previously unproven route.
pub fn apply_session_route_confirmation_plan(
    config_snapshot: &ResolvedRouteConfigSnapshot,
    session: &mut Session,
    plan: SessionRouteResumePlan,
    recovery_binding: &str,
    quiescence: SessionRouteMutationPermit,
) -> Result<SessionRouteResumeOutcome, SessionRouteResumeError> {
    let _mutation_guard = quiescence.enter(session.session_scope_id())?;
    let SessionRouteResumePlan::NeedsConfirmation {
        provider_name,
        source_route,
        target_route,
        target_egress_trust_binding,
        ..
    } = plan
    else {
        return Err(SessionRouteResumeError::PlanNotApplicable);
    };
    let frontier_binding = durable_session_frontier_binding(session);
    let authority_generation_binding =
        session_route_authority_generation_binding(session.entries());
    if route_recovery_binding(
        session.session_scope_id(),
        &source_route,
        &frontier_binding,
        &authority_generation_binding,
        config_snapshot,
    ) != recovery_binding
    {
        return Err(SessionRouteResumeError::SnapshotStale);
    }
    let current = config_snapshot
        .resolve_model_ref(&target_route.model_ref)
        .map_err(|_| SessionRouteResumeError::SnapshotStale)?;
    if current.provider_name != provider_name
        || current.route != target_route
        || current.egress_trust_binding != target_egress_trust_binding
        || session.resolved_model_route() != Some(&source_route)
    {
        return Err(SessionRouteResumeError::SessionRouteStale);
    }
    session
        .select_model_route_with_trust(provider_name, target_route, target_egress_trust_binding)
        .map_err(SessionRouteResumeError::Commit)?;
    Ok(SessionRouteResumeOutcome {
        status: SessionRouteResumeStatus::Applied,
        private_state_reset: true,
    })
}

/// Applies one explicit user-selected route at a proven quiescent session boundary.
pub fn apply_explicit_session_route_selection(
    config_snapshot: &ResolvedRouteConfigSnapshot,
    session: &mut Session,
    provider_name: &str,
    selected_route: ResolvedModelRoute,
    quiescence: SessionRouteMutationPermit,
) -> Result<SessionRouteResumeOutcome, SessionRouteResumeError> {
    let _mutation_guard = quiescence.enter(session.session_scope_id())?;
    let current = config_snapshot
        .resolve_model_ref(&selected_route.model_ref)
        .map_err(|_| SessionRouteResumeError::SnapshotStale)?;
    if current.provider_name != provider_name || current.route != selected_route {
        return Err(SessionRouteResumeError::SnapshotStale);
    }
    if session.resolved_model_route() == Some(&selected_route)
        && session.route_egress_trust_binding() == Some(current.egress_trust_binding.clone())
    {
        return Ok(SessionRouteResumeOutcome {
            status: SessionRouteResumeStatus::AlreadyApplied,
            private_state_reset: false,
        });
    }
    session
        .select_model_route_with_trust(
            provider_name.to_owned(),
            selected_route,
            current.egress_trust_binding,
        )
        .map_err(SessionRouteResumeError::Commit)?;
    Ok(SessionRouteResumeOutcome {
        status: SessionRouteResumeStatus::Applied,
        private_state_reset: true,
    })
}

/// Loads safe session truth and returns a pure decision without requiring provider readiness.
pub fn inspect_session_for_route_resume(
    root_config: &RootConfig,
    fallback_route: &ResolvedModelRoute,
    store: JsonlSessionStore,
) -> Result<InspectedSessionRouteResume> {
    let config_snapshot = ResolvedRouteConfigSnapshot::from_root_config(root_config);
    let (provider_name, _, fallback_trust) = config_snapshot
        .resolved_route(&fallback_route.model_ref)
        .ok_or_else(|| anyhow::anyhow!("model_route_not_configured"))?;
    let session = Session::load_from_store_with_route_and_trust(
        provider_name,
        fallback_route.model_ref.model_id.clone(),
        Some(fallback_route.clone()),
        Some(fallback_trust),
        store,
    )?;
    let persisted = session.resolved_model_route().cloned().ok_or_else(|| {
        anyhow::anyhow!("session_route_missing: start a new session or select a replacement route")
    })?;
    let plan = plan_session_route_resume(
        &config_snapshot,
        &SessionRouteResumeInput {
            route: persisted.clone(),
            egress_trust_binding: session.route_egress_trust_binding(),
        },
    );
    let frontier_binding = durable_session_frontier_binding(&session);
    let authority_generation_binding =
        session_route_authority_generation_binding(session.entries());
    let recovery_binding = route_recovery_binding(
        session.session_scope_id(),
        &persisted,
        &frontier_binding,
        &authority_generation_binding,
        &config_snapshot,
    );
    Ok(InspectedSessionRouteResume {
        session,
        config_snapshot,
        plan,
        recovery_binding,
    })
}

/// Loads an exact session route without granting route-mutation authority.
///
/// Confirmation, replacement, setup, or automatic-rebind dispositions are returned as stable
/// errors. Controllers that may mutate a route must use the attachment-aware loader so
/// cross-process ownership and process-local execution quiescence are both proven.
pub fn load_session_for_route_resume(
    root_config: &RootConfig,
    fallback_route: &ResolvedModelRoute,
    store: JsonlSessionStore,
) -> std::result::Result<Session, SessionRouteLoadError> {
    load_session_for_route_resume_with_directive(root_config, fallback_route, store, None, None)
}

/// Loads a session through the legacy non-mutating compatibility surface.
///
/// Recovery confirmation and explicit replacement require the attachment-aware overload and fail
/// closed here. Keeping this wrapper exact-only prevents external callers from fabricating an
/// authority that is disconnected from the controller attachment and its live execution owners.
pub fn load_session_for_route_resume_with_directive(
    root_config: &RootConfig,
    fallback_route: &ResolvedModelRoute,
    store: JsonlSessionStore,
    recovery_confirmation: Option<&str>,
    explicit_selection: Option<(&str, &ResolvedModelRoute)>,
) -> std::result::Result<Session, SessionRouteLoadError> {
    load_session_for_route_resume_with_directive_and_attachment(
        root_config,
        fallback_route,
        store,
        recovery_confirmation,
        explicit_selection,
        None,
    )
}

/// Attachment-aware route load used by interactive controllers. All execution owners and route
/// mutations under the attachment share one session-scoped authority.
pub fn load_session_for_route_resume_with_directive_and_attachment(
    root_config: &RootConfig,
    fallback_route: &ResolvedModelRoute,
    store: JsonlSessionStore,
    recovery_confirmation: Option<&str>,
    explicit_selection: Option<(&str, &ResolvedModelRoute)>,
    attachment: Option<&crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> std::result::Result<Session, SessionRouteLoadError> {
    load_session_for_route_resume_with_directive_and_attachment_transition(
        root_config,
        fallback_route,
        store,
        recovery_confirmation,
        explicit_selection,
        attachment,
    )
    .map(|outcome| outcome.session)
}

/// Loads a session and returns the exact bounded route transition receipt.
pub fn load_session_for_route_resume_with_directive_and_attachment_transition(
    root_config: &RootConfig,
    fallback_route: &ResolvedModelRoute,
    store: JsonlSessionStore,
    recovery_confirmation: Option<&str>,
    explicit_selection: Option<(&str, &ResolvedModelRoute)>,
    attachment: Option<&crate::interactive_session_attachment::InteractiveSessionAttachmentLease>,
) -> std::result::Result<SessionRouteLoadOutcome, SessionRouteLoadError> {
    let inspected = inspect_session_for_route_resume(root_config, fallback_route, store)
        .map_err(SessionRouteLoadError::Unavailable)?;
    let InspectedSessionRouteResume {
        mut session,
        config_snapshot,
        plan,
        recovery_binding,
    } = inspected;
    let authority = attachment
        .map(|attachment| {
            attachment
                .route_mutation_authority(session.session_scope_id())
                .map_err(SessionRouteLoadError::Unavailable)
        })
        .transpose()?;
    let mutation_authority = || {
        authority.as_ref().ok_or_else(|| {
            SessionRouteLoadError::Unavailable(anyhow::anyhow!(
                "session_route_mutation_requires_attachment"
            ))
        })
    };
    let mut transition_kind = SessionRouteTransitionKind::Exact;
    let mut remote_context_reset = false;
    if let Some((provider_name, selected_route)) = explicit_selection {
        match &plan {
            SessionRouteResumePlan::NeedsConfirmation { reason, .. }
                if recovery_confirmation != Some(recovery_binding.as_str()) =>
            {
                return Err(SessionRouteLoadError::ConfirmationRequired {
                    reason: *reason,
                    recovery_binding,
                });
            }
            SessionRouteResumePlan::NeedsReplacement { reason, .. }
                if recovery_confirmation != Some(recovery_binding.as_str()) =>
            {
                return Err(SessionRouteLoadError::SelectionRequired {
                    reason: *reason,
                    recovery_binding,
                });
            }
            SessionRouteResumePlan::NeedsSetup { reason }
                if recovery_confirmation != Some(recovery_binding.as_str()) =>
            {
                return Err(SessionRouteLoadError::SetupRequired {
                    reason: *reason,
                    recovery_binding,
                });
            }
            _ => {}
        }
        let permit = mutation_authority()?
            .issue_quiescence_permit()
            .map_err(|error| route_authority_load_error(error, &recovery_binding))?;
        let outcome = apply_explicit_session_route_selection(
            &config_snapshot,
            &mut session,
            provider_name,
            selected_route.clone(),
            permit,
        )
        .map_err(|error| SessionRouteLoadError::Unavailable(anyhow::Error::new(error)))?;
        transition_kind = SessionRouteTransitionKind::ExplicitlyConfirmed;
        remote_context_reset = outcome.private_state_reset;
    } else {
        match plan {
            SessionRouteResumePlan::Exact { .. } => {}
            plan @ SessionRouteResumePlan::RebindCurrentModel { .. } => {
                let permit = mutation_authority()?
                    .issue_quiescence_permit()
                    .map_err(|error| route_authority_load_error(error, &recovery_binding))?;
                let outcome =
                    apply_session_route_resume_plan(&config_snapshot, &mut session, plan, permit)
                        .map_err(|error| {
                        SessionRouteLoadError::Unavailable(anyhow::Error::new(error))
                    })?;
                transition_kind = SessionRouteTransitionKind::Rebound;
                remote_context_reset = outcome.private_state_reset;
            }
            plan @ SessionRouteResumePlan::NeedsConfirmation { reason, .. } => {
                if recovery_confirmation == Some(recovery_binding.as_str()) {
                    let permit = mutation_authority()?
                        .issue_quiescence_permit()
                        .map_err(|error| route_authority_load_error(error, &recovery_binding))?;
                    let outcome = apply_session_route_confirmation_plan(
                        &config_snapshot,
                        &mut session,
                        plan,
                        &recovery_binding,
                        permit,
                    )
                    .map_err(|error| {
                        SessionRouteLoadError::Unavailable(anyhow::Error::new(error))
                    })?;
                    transition_kind = SessionRouteTransitionKind::ExplicitlyConfirmed;
                    remote_context_reset = outcome.private_state_reset;
                } else {
                    return Err(SessionRouteLoadError::ConfirmationRequired {
                        reason,
                        recovery_binding,
                    });
                }
            }
            SessionRouteResumePlan::NeedsReplacement { reason, .. } => {
                return Err(SessionRouteLoadError::SelectionRequired {
                    reason,
                    recovery_binding,
                });
            }
            SessionRouteResumePlan::NeedsSetup { reason } => {
                return Err(SessionRouteLoadError::SetupRequired {
                    reason,
                    recovery_binding,
                });
            }
        }
    }
    if session
        .resolved_model_route()
        .is_none_or(|route| route.model_ref.model_id != session.model_name())
    {
        return Err(SessionRouteLoadError::Unavailable(anyhow::anyhow!(
            "session_route_drift: durable model identity does not match its selected route"
        )));
    }
    let installed_route = session.resolved_model_route();
    Ok(SessionRouteLoadOutcome {
        transition: SessionRouteTransitionView {
            kind: transition_kind,
            connection_id: installed_route
                .map(|route| route.model_ref.connection_id.as_str().to_owned()),
            model_id: installed_route.map(|route| route.model_ref.model_id.clone()),
            remote_context_reset,
        },
        session,
    })
}

fn route_authority_load_error(
    error: SessionRouteAuthorityError,
    recovery_binding: &str,
) -> SessionRouteLoadError {
    match error {
        SessionRouteAuthorityError::ActiveOwners
        | SessionRouteAuthorityError::TransitionInProgress => SessionRouteLoadError::WriterBusy {
            recovery_binding: recovery_binding.to_owned(),
        },
        other => SessionRouteLoadError::Unavailable(anyhow::Error::new(other)),
    }
}

/// Computes an opaque binding for the egress origins and tenant-routing options of a connection.
#[must_use]
pub fn connection_egress_trust_binding(
    connection: &ProviderConnectionConfig,
) -> RouteEgressTrustBinding {
    let mut material = BTreeMap::<String, String>::new();
    material.insert(
        "provider_family".to_owned(),
        connection.provider.as_str().to_owned(),
    );
    material.insert(
        "protocol".to_owned(),
        connection.protocol.as_str().to_owned(),
    );
    material.insert(
        "origin".to_owned(),
        normalized_network_origin(&connection.base_url),
    );
    collect_trust_boundary_options(&connection.options, "options", &mut material);
    let encoded = serde_json::to_vec(&material).expect("trust material is serializable");
    RouteEgressTrustBinding::new(stable_route_digest(&[encoded.as_slice()]))
        .expect("sha256 trust binding satisfies the durable contract")
}

fn collect_trust_boundary_options(
    value: &Value,
    path: &str,
    material: &mut BTreeMap<String, String>,
) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                if option_key_changes_egress_destination(key) {
                    if let Some(raw_url) = child.as_str() {
                        material.insert(child_path.clone(), normalized_network_origin(raw_url));
                    } else {
                        material.insert(child_path.clone(), canonical_json(child));
                    }
                } else if option_key_changes_tenant_boundary(key) {
                    material.insert(child_path.clone(), canonical_json(child));
                }
                collect_trust_boundary_options(child, &child_path, material);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                collect_trust_boundary_options(child, &format!("{path}[{index}]"), material);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn option_key_changes_egress_destination(key: &str) -> bool {
    let key = key.trim().to_ascii_lowercase().replace('-', "_");
    key == "url" || key.ends_with("_url") || key.contains("endpoint")
}

fn option_key_changes_tenant_boundary(key: &str) -> bool {
    let key = key.trim().to_ascii_lowercase().replace('-', "_");
    matches!(
        key.as_str(),
        "organization"
            | "organization_id"
            | "org"
            | "org_id"
            | "project"
            | "project_id"
            | "tenant"
            | "tenant_id"
            | "account"
            | "account_id"
    )
}

fn normalized_network_origin(raw_url: &str) -> String {
    let Ok(url) = url::Url::parse(raw_url) else {
        return "invalid-origin".to_owned();
    };
    let Some(host) = url.host_str() else {
        return "invalid-origin".to_owned();
    };
    let port = url.port_or_known_default().unwrap_or_default();
    format!(
        "{}://{}:{port}",
        url.scheme().to_ascii_lowercase(),
        host.to_ascii_lowercase()
    )
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let sorted = object
                .iter()
                .map(|(key, value)| (key.clone(), canonical_json(value)))
                .collect::<BTreeMap<_, _>>();
            serde_json::to_string(&sorted).unwrap_or_default()
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

fn stable_route_digest(parts: &[&[u8]]) -> String {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    format!("sha256:{:x}", digest.finalize())
}

fn route_snapshot_binding(
    mode: ConfigMode,
    default_model: Option<&ModelRef>,
    connections: &BTreeMap<ConnectionId, ResolvedRouteSnapshotConnection>,
) -> String {
    let mut material = format!("sigil-route-snapshot-v1\n{mode:?}\n");
    if let Some(default_model) = default_model {
        material.push_str(default_model.connection_id.as_str());
        material.push('/');
        material.push_str(&default_model.model_id);
        material.push('\n');
    }
    for (connection_id, connection) in connections {
        use std::fmt::Write as _;
        let _ = writeln!(
            material,
            "{}|{}|{}|{}|{}",
            connection_id,
            connection.provider_family,
            connection.protocol,
            connection.semantic_fingerprint,
            connection.egress_trust_binding.as_str(),
        );
    }
    stable_route_digest(&[material.as_bytes()])
}

fn route_recovery_binding(
    session_scope_id: &str,
    source_route: &ResolvedModelRoute,
    session_frontier_binding: &str,
    route_authority_generation_binding: &str,
    config_snapshot: &ResolvedRouteConfigSnapshot,
) -> String {
    stable_route_digest(&[
        b"sigil-session-route-recovery-v3",
        session_scope_id.as_bytes(),
        source_route.model_ref.connection_id.as_str().as_bytes(),
        source_route.model_ref.model_id.as_bytes(),
        source_route.semantic_fingerprint.as_bytes(),
        session_frontier_binding.as_bytes(),
        route_authority_generation_binding.as_bytes(),
        config_snapshot.binding().as_bytes(),
    ])
}

fn durable_session_frontier_binding(session: &Session) -> String {
    session_route_frontier_binding(session.entries())
}

/// Computes an opaque digest of the exact durable session frontier used by route recovery.
#[must_use]
pub fn session_route_frontier_binding(entries: &[sigil_kernel::SessionLogEntry]) -> String {
    let encoded = serde_json::to_vec(entries).unwrap_or_default();
    stable_route_digest(&[b"sigil-session-route-frontier-v1", encoded.as_slice()])
}

/// Computes an opaque generation for the latest durable route-mutation boundary.
///
/// This is distinct from the whole-session frontier so a recovery command is explicitly bound to
/// the route authority generation it reviewed as well as to every intervening durable append.
#[must_use]
pub fn session_route_authority_generation_binding(
    entries: &[sigil_kernel::SessionLogEntry],
) -> String {
    let mut generation = 0_u64;
    let mut latest_boundary = Vec::new();
    for entry in entries {
        if matches!(
            entry,
            sigil_kernel::SessionLogEntry::Control(
                sigil_kernel::ControlEntry::SessionIdentity { .. }
                    | sigil_kernel::ControlEntry::SessionModelSelected { .. }
                    | sigil_kernel::ControlEntry::SessionRouteRebound { .. }
            )
        ) {
            generation = generation.saturating_add(1);
            latest_boundary = serde_json::to_vec(entry).unwrap_or_default();
        }
    }
    stable_route_digest(&[
        b"sigil-session-route-authority-generation-v1",
        &generation.to_le_bytes(),
        latest_boundary.as_slice(),
    ])
}

pub fn resolve_default_model_route(
    root_config: &RootConfig,
) -> std::result::Result<(String, ResolvedModelRoute), ResolvedRouteError> {
    let loaded = load_provider_connections(root_config);
    if loaded.mode != ConfigMode::V2 {
        return Err(ResolvedRouteError::ConnectionConfigInvalid);
    }
    let model_ref = loaded
        .default_model
        .as_ref()
        .ok_or(ResolvedRouteError::NotConfigured)?;
    resolve_model_route(root_config, model_ref)
}

pub fn resolve_model_route(
    root_config: &RootConfig,
    model_ref: &ModelRef,
) -> std::result::Result<(String, ResolvedModelRoute), ResolvedRouteError> {
    let loaded = load_provider_connections(root_config);
    if loaded.mode != ConfigMode::V2 {
        return Err(ResolvedRouteError::ConnectionConfigInvalid);
    }
    if loaded.issues.iter().any(|issue| {
        issue.connection_id.is_none()
            || issue.connection_id.as_deref() == Some(model_ref.connection_id.as_str())
    }) {
        return Err(ResolvedRouteError::ConnectionConfigInvalid);
    }
    let connection = loaded
        .connections
        .get(&model_ref.connection_id)
        .ok_or(ResolvedRouteError::ConnectionNotFound)?;
    let route = ResolvedModelRoute::new(
        model_ref.clone(),
        connection.config.provider.as_str(),
        connection.config.protocol.as_str(),
        connection_semantic_fingerprint(&connection.config),
    )
    .map_err(|_| ResolvedRouteError::ConnectionConfigInvalid)?;
    Ok((runtime_provider_name(&connection.config).to_owned(), route))
}

pub fn validate_persisted_model_route(
    root_config: &RootConfig,
    persisted: &ResolvedModelRoute,
) -> std::result::Result<String, ResolvedRouteError> {
    let (provider_name, current) = resolve_model_route(root_config, &persisted.model_ref)?;
    if current.provider_family != persisted.provider_family
        || current.protocol != persisted.protocol
        || current.semantic_fingerprint != persisted.semantic_fingerprint
    {
        return Err(ResolvedRouteError::SemanticDrift);
    }
    Ok(provider_name)
}

#[must_use]
pub fn runtime_provider_name(connection: &ProviderConnectionConfig) -> &'static str {
    match (connection.provider, connection.protocol) {
        (ProviderFamily::DeepSeek, ProviderProtocol::DeepSeek) => "deepseek",
        (ProviderFamily::OpenAi, ProviderProtocol::OpenAiResponses)
        | (ProviderFamily::Custom, ProviderProtocol::OpenAiResponses) => "openai_responses",
        (ProviderFamily::Custom, ProviderProtocol::OpenAiChatCompletions) => "openai_compat",
        (ProviderFamily::Anthropic, ProviderProtocol::AnthropicMessages) => "anthropic",
        (ProviderFamily::Gemini, ProviderProtocol::GeminiGenerateContent) => "gemini",
        _ => "unsupported",
    }
}

pub fn ensure_route_is_current(root_config: &RootConfig, route: &ResolvedModelRoute) -> Result<()> {
    validate_persisted_model_route(root_config, route)
        .map(|_| ())
        .map_err(anyhow::Error::new)
}
