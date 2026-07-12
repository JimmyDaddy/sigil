use std::sync::Arc;

use async_trait::async_trait;
use sigil_kernel::{
    DEFAULT_WEB_URL_CAPABILITY_TTL_MS, EgressDataCategory, EgressDisclosureKind,
    EgressNetworkRoute, ExternalEvidenceLevel, ExternalSourceRecord, SourceCacheStatus,
    SourceFreshness, UserUrlCapabilityLookupError, UserUrlCapabilityRegistrar,
    UserUrlCapabilityRegistration, WebBudgetReservation, WebFetchTransportAuthorization,
    WebUrlProvenanceKind, canonical_web_url_persistence_projection, sha256_hex,
};
use sigil_tools_builtin::{
    WebFetchAuthorizedDialPlan, WebFetchError, WebFetchFetchedResponse, WebFetchFormat,
    WebFetchHopResult, WebFetchLimits, WebFetchTransport,
};
use thiserror::Error;
use url::Url;
use uuid::Uuid;

use crate::{
    EgressOrderingCoordinator, EgressOrderingError, WebDestinationError, WebDestinationGuard,
    WebDestinationResolver, current_unix_time_ms,
};

const MAX_SAME_ORIGIN_REDIRECTS: usize = 5;

#[derive(Debug, Clone)]
pub struct WebFetchExecutionRequest {
    pub session_scope_id: String,
    pub source_id: String,
    pub root_run_id: String,
    pub authorization_id: String,
    pub disclosure_id: String,
    pub attempt_id: String,
    pub route_fingerprint: String,
    pub profile_config_proxy_fingerprint: String,
    pub surface: String,
    pub display_name: String,
    pub output_durable_entry_id: String,
    pub originating_call_id: String,
    pub retrieved_at: String,
    pub limits: WebFetchLimits,
    pub format: WebFetchFormat,
}

pub enum WebFetchExecutionOutcome {
    Fetched {
        response: WebFetchFetchedResponse,
        source: Box<ExternalSourceRecord>,
        url_registration: UserUrlCapabilityRegistration,
    },
    RedirectRequiresCapability {
        safe_display_url: String,
        url_registration: UserUrlCapabilityRegistration,
    },
}

impl std::fmt::Debug for WebFetchExecutionOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fetched {
                response,
                source,
                url_registration,
            } => formatter
                .debug_struct("Fetched")
                .field("status", &response.status)
                .field("model_bytes", &response.model_bytes)
                .field("truncated", &response.truncated)
                .field("source_id", &source.source_id)
                .field("url_registration", url_registration)
                .finish(),
            Self::RedirectRequiresCapability {
                safe_display_url,
                url_registration,
            } => formatter
                .debug_struct("RedirectRequiresCapability")
                .field("safe_display_url", safe_display_url)
                .field("url_registration", url_registration)
                .finish(),
        }
    }
}

#[derive(Debug, Error)]
pub enum WebFetchExecutionError {
    #[error("webfetch URL capability lookup failed: {0}")]
    Capability(#[from] UserUrlCapabilityLookupError),
    #[error("webfetch URL capability contains an invalid exact URL")]
    InvalidCapabilityUrl,
    #[error("webfetch exceeded the same-origin redirect limit")]
    RedirectLimitExceeded,
    #[error("webfetch redirect target is invalid")]
    InvalidRedirect,
    #[error(transparent)]
    Ordering(#[from] EgressOrderingError),
    #[error(transparent)]
    Destination(#[from] WebDestinationError),
    #[error(transparent)]
    Transport(#[from] WebFetchError),
    #[error("webfetch provenance normalization failed")]
    Provenance(#[source] anyhow::Error),
}

#[async_trait]
pub trait WebFetchHopTransport: Send + Sync {
    async fn fetch_once(
        &self,
        plan: &WebFetchAuthorizedDialPlan,
        reservation: &mut WebBudgetReservation,
        limits: WebFetchLimits,
        format: WebFetchFormat,
    ) -> Result<WebFetchHopResult, WebFetchError>;
}

#[async_trait]
impl WebFetchHopTransport for WebFetchTransport {
    async fn fetch_once(
        &self,
        plan: &WebFetchAuthorizedDialPlan,
        reservation: &mut WebBudgetReservation,
        limits: WebFetchLimits,
        format: WebFetchFormat,
    ) -> Result<WebFetchHopResult, WebFetchError> {
        WebFetchTransport::fetch_once(self, plan, reservation, limits, format).await
    }
}

pub struct WebFetchExecutor<R, T> {
    capabilities: Arc<dyn UserUrlCapabilityRegistrar>,
    ordering: EgressOrderingCoordinator,
    destination_guard: WebDestinationGuard<R>,
    transport: T,
}

impl<R, T> std::fmt::Debug for WebFetchExecutor<R, T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebFetchExecutor")
            .field("capabilities", &"configured")
            .finish_non_exhaustive()
    }
}

impl<R, T> WebFetchExecutor<R, T>
where
    R: WebDestinationResolver,
    T: WebFetchHopTransport,
{
    #[must_use]
    pub fn new(
        capabilities: Arc<dyn UserUrlCapabilityRegistrar>,
        ordering: EgressOrderingCoordinator,
        destination_guard: WebDestinationGuard<R>,
        transport: T,
    ) -> Self {
        Self {
            capabilities,
            ordering,
            destination_guard,
            transport,
        }
    }

    pub async fn execute(
        &self,
        request: WebFetchExecutionRequest,
        mut reservation: WebBudgetReservation,
        admission_is_live: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<WebFetchExecutionOutcome, WebFetchExecutionError> {
        let capability = self
            .capabilities
            .resolve(&request.session_scope_id, &request.source_id)?;
        let mut current_url = Url::parse(capability.raw_canonical_url().expose_secret())
            .map_err(|_| WebFetchExecutionError::InvalidCapabilityUrl)?;

        for hop in 0..=MAX_SAME_ORIGIN_REDIRECTS {
            let preview = self.destination_guard.preview(current_url.clone())?;
            let route = if preview.is_proxy_remote() {
                EgressNetworkRoute::ProxyRemote
            } else {
                EgressNetworkRoute::Direct
            };
            let authorization_id = hop_identity(&request.authorization_id, hop);
            let disclosure_id = hop_identity(&request.disclosure_id, hop);
            let authorization = WebFetchTransportAuthorization {
                record_id: format!("webfetch-authorization-record-{authorization_id}"),
                root_run_id: request.root_run_id.clone(),
                authorization_id,
                disclosure_id: disclosure_id.clone(),
                route_fingerprint: request.route_fingerprint.clone(),
                profile_config_proxy_fingerprint: request.profile_config_proxy_fingerprint.clone(),
                route,
                safe_logical_destination: preview.safe_logical_destination().to_owned(),
                safe_transport_destination: preview.safe_transport_destination().to_owned(),
            };
            let disclosure = sigil_kernel::PreEgressDisclosure::new(
                EgressDisclosureKind::Transport,
                None,
                disclosure_id,
                request.surface.clone(),
                request.display_name.clone(),
                request.route_fingerprint.clone(),
                request.profile_config_proxy_fingerprint.clone(),
                preview.safe_logical_destination().to_owned(),
                preview.safe_transport_destination().to_owned(),
                route,
                vec![EgressDataCategory::ConnectionMetadata],
            )
            .map_err(EgressOrderingError::Audit)?;
            let permit = self
                .ordering
                .authorize_webfetch_transport(
                    authorization,
                    disclosure,
                    reservation,
                    admission_is_live,
                )
                .await?;
            let attempt_id = if hop == 0 {
                request.attempt_id.clone()
            } else {
                hop_identity(&request.attempt_id, hop)
            };
            reservation = permit.begin_attempt(&attempt_id, preview.safe_host())?;
            let plan = self.destination_guard.authorize_preview(preview).await?;
            reservation
                .commit_call()
                .map_err(EgressOrderingError::Budget)?;
            match self
                .transport
                .fetch_once(&plan, &mut reservation, request.limits, request.format)
                .await?
            {
                WebFetchHopResult::Fetched(response) => {
                    return fetched_outcome(&request, current_url, response);
                }
                WebFetchHopResult::Redirect { location, .. } => {
                    let next_url = current_url
                        .join(location.expose_secret())
                        .map_err(|_| WebFetchExecutionError::InvalidRedirect)?;
                    if !same_origin(&current_url, &next_url) {
                        return redirect_outcome(&request, next_url);
                    }
                    if hop == MAX_SAME_ORIGIN_REDIRECTS {
                        return Err(WebFetchExecutionError::RedirectLimitExceeded);
                    }
                    current_url = next_url;
                }
            }
        }
        Err(WebFetchExecutionError::RedirectLimitExceeded)
    }
}

fn fetched_outcome(
    request: &WebFetchExecutionRequest,
    final_url: Url,
    response: WebFetchFetchedResponse,
) -> Result<WebFetchExecutionOutcome, WebFetchExecutionError> {
    let source = ExternalSourceRecord::from_remote_candidate(
        request.session_scope_id.clone(),
        None,
        ExternalEvidenceLevel::FetchedPage,
        final_url.as_str(),
        "builtin_webfetch",
        response.title.clone(),
        None,
        request.retrieved_at.clone(),
        Some(sha256_hex(response.body.as_bytes())),
        None,
        SourceFreshness::Fresh,
        SourceCacheStatus::NotApplicable,
        canonical_web_url_persistence_projection(final_url.as_str())
            .map_err(WebFetchExecutionError::Provenance)?
            .restart_policy,
    )
    .map_err(WebFetchExecutionError::Provenance)?;
    let url_registration = registration_for_url(
        request,
        source.source_id.clone(),
        final_url,
        WebUrlProvenanceKind::PriorWebFetch,
    )?;
    Ok(WebFetchExecutionOutcome::Fetched {
        response,
        source: Box::new(source),
        url_registration,
    })
}

fn redirect_outcome(
    request: &WebFetchExecutionRequest,
    redirect_url: Url,
) -> Result<WebFetchExecutionOutcome, WebFetchExecutionError> {
    let source_id = format!("src_{}", Uuid::new_v4().simple());
    let registration = registration_for_url(
        request,
        source_id,
        redirect_url,
        WebUrlProvenanceKind::RedirectTarget,
    )?;
    Ok(WebFetchExecutionOutcome::RedirectRequiresCapability {
        safe_display_url: registration.safe_display_url.clone(),
        url_registration: registration,
    })
}

fn registration_for_url(
    request: &WebFetchExecutionRequest,
    source_id: String,
    url: Url,
    provenance: WebUrlProvenanceKind,
) -> Result<UserUrlCapabilityRegistration, WebFetchExecutionError> {
    let projection = canonical_web_url_persistence_projection(url.as_str())
        .map_err(WebFetchExecutionError::Provenance)?;
    let issued_at_ms = current_unix_time_ms();
    Ok(UserUrlCapabilityRegistration {
        source_id,
        durable_entry_id: request.output_durable_entry_id.clone(),
        raw_canonical_url: projection.raw_canonical_url,
        safe_display_url: projection.safe_display_url,
        restart_policy: projection.restart_policy,
        replayable_canonical_url: projection.replayable_canonical_url,
        originating_call_id: Some(request.originating_call_id.clone()),
        provenance,
        issued_at_ms,
        expires_at_ms: issued_at_ms.saturating_add(DEFAULT_WEB_URL_CAPABILITY_TTL_MS),
    })
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn hop_identity(base: &str, hop: usize) -> String {
    if hop == 0 {
        base.to_owned()
    } else {
        format!("{base}-hop-{hop}")
    }
}

#[cfg(test)]
#[path = "tests/webfetch_executor_tests.rs"]
mod tests;
