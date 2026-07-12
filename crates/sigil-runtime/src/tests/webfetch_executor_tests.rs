use std::{
    collections::VecDeque,
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, DurableEventType,
    EgressDisclosurePresenter, ExternalEvidenceLevel, JsonlSessionStore, PreEgressDisclosure,
    SecretString, Session, SessionStreamRecord, UserUrlCapabilityLookupError,
    UserUrlCapabilityRegistrar, UserUrlCapabilityRegistration, WebBudgetReservationKind,
    WebBudgetReservationRequest, WebTaskTreeBudget, WebTaskTreeBudgetLimits, WebUrlProvenanceKind,
    canonical_web_url_persistence_projection,
};
use sigil_tools_builtin::{
    WebFetchFetchedResponse, WebFetchNetworkGuard, WebFetchTransportSecurity,
};
use tempfile::tempdir;

use super::*;
use crate::{
    ProxyEnvironment, WebDestinationGuardPolicy, WebDestinationResolver, WebUrlCapabilityStore,
};

const SESSION: &str = "webfetch-session";
const SOURCE: &str = "src_00000000000000000000000000000001";

#[derive(Debug, Clone)]
struct FakeResolver {
    answers: Arc<Mutex<VecDeque<Vec<IpAddr>>>>,
    calls: Arc<AtomicUsize>,
}

impl FakeResolver {
    fn new(answers: impl IntoIterator<Item = Vec<IpAddr>>) -> Self {
        Self {
            answers: Arc::new(Mutex::new(answers.into_iter().collect())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WebDestinationResolver for FakeResolver {
    async fn resolve_all(
        &self,
        _host: &str,
        _port: u16,
    ) -> Result<Vec<IpAddr>, WebDestinationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answers
            .lock()
            .expect("resolver should lock")
            .pop_front()
            .ok_or(WebDestinationError::ResolutionFailed)
    }
}

#[derive(Debug, Clone)]
struct FakeTransport {
    results: Arc<Mutex<VecDeque<WebFetchHopResult>>>,
    calls: Arc<AtomicUsize>,
}

impl FakeTransport {
    fn new(results: impl IntoIterator<Item = WebFetchHopResult>) -> Self {
        Self {
            results: Arc::new(Mutex::new(results.into_iter().collect())),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WebFetchHopTransport for FakeTransport {
    async fn fetch_once(
        &self,
        _plan: &WebFetchAuthorizedDialPlan,
        _reservation: &mut WebBudgetReservation,
        _limits: WebFetchLimits,
        _format: WebFetchFormat,
    ) -> Result<WebFetchHopResult, WebFetchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.results
            .lock()
            .expect("transport results should lock")
            .pop_front()
            .ok_or(WebFetchError::InvalidDialPlan(
                "missing test result".to_owned(),
            ))
    }
}

struct TestPresenter {
    stale: Option<PreEgressDisclosure>,
}

#[async_trait]
impl EgressDisclosurePresenter for TestPresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        self.stale
            .as_ref()
            .unwrap_or(&disclosure)
            .presentation_receipt("webfetch-test-sink-v1")
    }
}

#[tokio::test]
async fn exact_capability_orders_durable_barrier_before_dns_and_returns_fetched_provenance() {
    let fixture = Fixture::new(Some(Arc::new(TestPresenter { stale: None })));
    let resolver = FakeResolver::new([vec![ip("93.184.216.34")]]);
    let transport = FakeTransport::new([fetched("safe fetched body")]);
    let executor = fixture.executor(resolver.clone(), transport.clone());
    let outcome = executor
        .execute(request(), reservation(), &|| true)
        .await
        .expect("fetch should complete");
    let WebFetchExecutionOutcome::Fetched {
        response,
        source,
        url_registration,
    } = outcome
    else {
        panic!("expected fetched outcome");
    };
    assert_eq!(response.body, "safe fetched body");
    assert_eq!(source.evidence_level, ExternalEvidenceLevel::FetchedPage);
    assert_eq!(source.origin, "builtin_webfetch");
    assert_eq!(url_registration.source_id, source.source_id);
    assert_eq!(
        url_registration.provenance,
        WebUrlProvenanceKind::PriorWebFetch
    );
    assert!(!format!("{url_registration:?}").contains("token=secret"));
    assert_eq!(resolver.calls(), 1);
    assert_eq!(transport.calls(), 1);

    let order = fixture.event_order();
    let authorization = position(&order, DurableEventType::WebFetchTransportAuthorization);
    let presented = position(&order, DurableEventType::EgressDisclosurePresented);
    assert!(authorization < presented);
}

#[tokio::test]
async fn missing_or_stale_presenter_and_source_mismatch_produce_zero_dns_and_transport() {
    let fixture = Fixture::new(None);
    let resolver = FakeResolver::new([]);
    let transport = FakeTransport::new([]);
    let result = fixture
        .executor(resolver.clone(), transport.clone())
        .execute(request(), reservation(), &|| true)
        .await;
    assert!(matches!(
        result,
        Err(WebFetchExecutionError::Ordering(
            EgressOrderingError::MissingPresenter
        ))
    ));
    assert_eq!(resolver.calls(), 0);
    assert_eq!(transport.calls(), 0);

    let stale_disclosure = PreEgressDisclosure::new(
        EgressDisclosureKind::Transport,
        None,
        "stale-disclosure",
        "tui",
        "Built-in WebFetch",
        "route-fingerprint",
        "profile-fingerprint",
        "https://example.test/",
        "https://example.test/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::ConnectionMetadata],
    )
    .expect("stale disclosure should build");
    let fixture = Fixture::new(Some(Arc::new(TestPresenter {
        stale: Some(stale_disclosure),
    })));
    let resolver = FakeResolver::new([]);
    let transport = FakeTransport::new([]);
    let result = fixture
        .executor(resolver.clone(), transport.clone())
        .execute(request(), reservation(), &|| true)
        .await;
    assert!(matches!(
        result,
        Err(WebFetchExecutionError::Ordering(
            EgressOrderingError::Audit(_)
        ))
    ));
    assert_eq!(resolver.calls(), 0);
    assert_eq!(transport.calls(), 0);

    let fixture = Fixture::new(Some(Arc::new(TestPresenter { stale: None })));
    let resolver = FakeResolver::new([]);
    let transport = FakeTransport::new([]);
    let mut mismatched = request();
    mismatched.source_id = "src_ffffffffffffffffffffffffffffffff".to_owned();
    let result = fixture
        .executor(resolver.clone(), transport.clone())
        .execute(mismatched, reservation(), &|| true)
        .await;
    assert!(matches!(
        result,
        Err(WebFetchExecutionError::Capability(
            UserUrlCapabilityLookupError::NotFound
        ))
    ));
    assert_eq!(resolver.calls(), 0);
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn transport_lifecycle_reservation_cannot_reach_webfetch_dns() {
    let fixture = Fixture::new(Some(Arc::new(TestPresenter { stale: None })));
    let resolver = FakeResolver::new([]);
    let transport = FakeTransport::new([]);
    let budget = budget();
    let reservation = budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "webfetch-transport-correlation".to_owned(),
            attempt_id: "webfetch-attempt".to_owned(),
            route_lease_id: "webfetch-route-lease".to_owned(),
            route_fingerprint: "route-fingerprint".to_owned(),
            kind: WebBudgetReservationKind::TransportLifecycle,
        })
        .expect("transport reservation should create");
    let result = fixture
        .executor(resolver.clone(), transport.clone())
        .execute(request(), reservation, &|| true)
        .await;
    assert!(matches!(
        result,
        Err(WebFetchExecutionError::Ordering(
            EgressOrderingError::BindingMismatch
        ))
    ));
    assert_eq!(resolver.calls(), 0);
    assert_eq!(transport.calls(), 0);
}

#[tokio::test]
async fn same_origin_redirect_repeats_barrier_dns_and_attempt() {
    let fixture = Fixture::new(Some(Arc::new(TestPresenter { stale: None })));
    let resolver = FakeResolver::new([vec![ip("93.184.216.34")], vec![ip("93.184.216.35")]]);
    let transport = FakeTransport::new([
        WebFetchHopResult::Redirect {
            status: 302,
            location: SecretString::new("/next?redirected=secret"),
        },
        fetched("redirected body"),
    ]);
    let budget = budget();
    let outcome = fixture
        .executor(resolver.clone(), transport.clone())
        .execute(request(), reservation_for(&budget), &|| true)
        .await
        .expect("same-origin redirect should complete");
    assert!(matches!(outcome, WebFetchExecutionOutcome::Fetched { .. }));
    assert_eq!(resolver.calls(), 2);
    assert_eq!(transport.calls(), 2);
    let snapshot = budget.snapshot().expect("snapshot should succeed");
    assert_eq!(snapshot.logical_calls, 1);
    assert_eq!(snapshot.network_attempts, 2);
    assert_eq!(
        fixture
            .event_order()
            .iter()
            .filter(|event| **event == DurableEventType::WebFetchTransportAuthorization)
            .count(),
        2
    );
}

#[tokio::test]
async fn cross_origin_or_https_downgrade_returns_a_new_capability_without_second_dns() {
    for location in [
        "https://other.example/path?token=redirect-secret",
        "http://example.test/path?token=downgrade-secret",
    ] {
        let fixture = Fixture::new(Some(Arc::new(TestPresenter { stale: None })));
        let resolver = FakeResolver::new([vec![ip("93.184.216.34")]]);
        let transport = FakeTransport::new([WebFetchHopResult::Redirect {
            status: 302,
            location: SecretString::new(location),
        }]);
        let outcome = fixture
            .executor(resolver.clone(), transport.clone())
            .execute(request(), reservation(), &|| true)
            .await
            .expect("cross-origin redirect should return a boundary");
        let WebFetchExecutionOutcome::RedirectRequiresCapability {
            safe_display_url,
            url_registration,
        } = outcome
        else {
            panic!("expected redirect capability boundary");
        };
        assert!(safe_display_url.contains("[redacted]"));
        assert!(!safe_display_url.contains("redirect-secret"));
        assert!(!safe_display_url.contains("downgrade-secret"));
        assert_eq!(
            url_registration.provenance,
            WebUrlProvenanceKind::RedirectTarget
        );
        assert_eq!(resolver.calls(), 1);
        assert_eq!(transport.calls(), 1);
    }
}

#[tokio::test]
async fn dns_rebinding_failure_after_durable_barrier_emits_zero_transport_bytes() {
    let fixture = Fixture::new(Some(Arc::new(TestPresenter { stale: None })));
    let resolver = FakeResolver::new([vec![ip("10.0.0.1")]]);
    let transport = FakeTransport::new([]);
    let result = fixture
        .executor(resolver.clone(), transport.clone())
        .execute(request(), reservation(), &|| true)
        .await;
    assert!(matches!(
        result,
        Err(WebFetchExecutionError::Destination(
            WebDestinationError::PrivateAddressDenied
        ))
    ));
    assert_eq!(resolver.calls(), 1);
    assert_eq!(transport.calls(), 0);
    let order = fixture.event_order();
    assert!(order.contains(&DurableEventType::WebFetchTransportAuthorization));
    assert!(order.contains(&DurableEventType::EgressDisclosurePresented));
}

struct Fixture {
    _temp: tempfile::TempDir,
    store: JsonlSessionStore,
    capabilities: Arc<WebUrlCapabilityStore>,
    ordering: EgressOrderingCoordinator,
}

impl Fixture {
    fn new(presenter: Option<Arc<dyn EgressDisclosurePresenter>>) -> Self {
        let temp = tempdir().expect("temp dir should create");
        let store = JsonlSessionStore::new(temp.path().join("session.jsonl"))
            .expect("session store should create");
        let session = Session::new("provider", "model").with_store(store.clone());
        let ordering = EgressOrderingCoordinator::new(
            session
                .egress_audit_recorder()
                .expect("recorder should create"),
            presenter,
        );
        let capabilities =
            Arc::new(WebUrlCapabilityStore::new(SESSION).expect("capability store should create"));
        let projection =
            canonical_web_url_persistence_projection("https://example.test/start?token=secret")
                .expect("URL projection should succeed");
        capabilities
            .stage(UserUrlCapabilityRegistration {
                source_id: SOURCE.to_owned(),
                durable_entry_id: "input-message".to_owned(),
                raw_canonical_url: projection.raw_canonical_url,
                safe_display_url: projection.safe_display_url,
                restart_policy: projection.restart_policy,
                replayable_canonical_url: projection.replayable_canonical_url,
                originating_call_id: None,
                provenance: WebUrlProvenanceKind::UserMessage,
                issued_at_ms: current_unix_time_ms(),
                expires_at_ms: u64::MAX,
            })
            .expect("capability should stage");
        capabilities
            .commit_message("input-message")
            .expect("capability should commit");
        Self {
            _temp: temp,
            store,
            capabilities,
            ordering,
        }
    }

    fn executor(
        &self,
        resolver: FakeResolver,
        transport: FakeTransport,
    ) -> WebFetchExecutor<FakeResolver, FakeTransport> {
        let capabilities: Arc<dyn UserUrlCapabilityRegistrar> = self.capabilities.clone();
        WebFetchExecutor::new(
            capabilities,
            self.ordering.clone(),
            WebDestinationGuard::new(
                resolver,
                WebDestinationGuardPolicy::default(),
                ProxyEnvironment::default(),
            ),
            transport,
        )
    }

    fn event_order(&self) -> Vec<DurableEventType> {
        self.store
            .read_event_records_writer()
            .expect("events should read")
            .into_iter()
            .filter_map(|record| match record {
                SessionStreamRecord::Stored(event) => event.event_kind(),
                SessionStreamRecord::Legacy { .. } => None,
            })
            .collect()
    }
}

fn request() -> WebFetchExecutionRequest {
    WebFetchExecutionRequest {
        session_scope_id: SESSION.to_owned(),
        source_id: SOURCE.to_owned(),
        root_run_id: "webfetch-root".to_owned(),
        authorization_id: "webfetch-authorization".to_owned(),
        disclosure_id: "webfetch-disclosure".to_owned(),
        attempt_id: "webfetch-attempt".to_owned(),
        route_fingerprint: "route-fingerprint".to_owned(),
        profile_config_proxy_fingerprint: "profile-fingerprint".to_owned(),
        surface: "tui".to_owned(),
        display_name: "Built-in WebFetch".to_owned(),
        output_durable_entry_id: "webfetch-output".to_owned(),
        originating_call_id: "webfetch-call".to_owned(),
        retrieved_at: "2026-07-11T12:00:00Z".to_owned(),
        limits: WebFetchLimits::default(),
        format: WebFetchFormat::Markdown,
    }
}

fn fetched(body: &str) -> WebFetchHopResult {
    WebFetchHopResult::Fetched(WebFetchFetchedResponse {
        status: 200,
        body: body.to_owned(),
        content_type: Some("text/plain; charset=utf-8".to_owned()),
        title: Some("Fixture title".to_owned()),
        wire_bytes: body.len(),
        decoded_bytes: body.len(),
        model_bytes: body.len(),
        truncated: false,
        transport_security: WebFetchTransportSecurity::DirectPinned,
        network_guard: WebFetchNetworkGuard::DirectAllAddressesPinned,
    })
}

fn budget() -> Arc<WebTaskTreeBudget> {
    WebTaskTreeBudget::new(
        "webfetch-root",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 4,
            max_client_search_calls: 4,
            max_hosted_requests: 4,
            max_network_attempts: 8,
            max_wire_bytes: 8 * 1024 * 1024,
            max_decoded_bytes: 8 * 1024 * 1024,
            max_model_bytes: 8 * 1024 * 1024,
            max_concurrent_requests: 4,
            max_attempts_per_host: 8,
        },
        None,
    )
    .expect("budget should create")
}

fn reservation() -> WebBudgetReservation {
    reservation_for(&budget())
}

fn reservation_for(budget: &Arc<WebTaskTreeBudget>) -> WebBudgetReservation {
    budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "webfetch-correlation".to_owned(),
            attempt_id: "webfetch-attempt".to_owned(),
            route_lease_id: "webfetch-route-lease".to_owned(),
            route_fingerprint: "route-fingerprint".to_owned(),
            kind: WebBudgetReservationKind::FetchCall,
        })
        .expect("reservation should create")
}

fn position(order: &[DurableEventType], event: DurableEventType) -> usize {
    order
        .iter()
        .position(|candidate| *candidate == event)
        .expect("event should exist")
}

fn ip(value: &str) -> IpAddr {
    value.parse().expect("IP should parse")
}
