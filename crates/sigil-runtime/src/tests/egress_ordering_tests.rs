use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use sigil_kernel::{
    ApprovalMode, DisclosurePresentationError, DisclosurePresentationReceipt, DurableEventType,
    EgressBindingOrigin, EgressDataCategory, EgressDisclosureKind, EgressDisclosurePresenter,
    EgressNetworkRoute, HostedAuthorizationScope, HostedToolAuthorization,
    HostedToolTerminalStatus, JsonlSessionStore, McpTransportAuthorization, PreEgressDisclosure,
    QueryEgressStarted, QueryEgressTerminalStatus, Session, SessionStreamRecord, WebBudgetByteKind,
    WebBudgetReservationKind, WebBudgetReservationRequest, WebQueryEgressClass, WebTaskTreeBudget,
    WebTaskTreeBudgetLimits,
};
use tempfile::tempdir;

use super::*;

struct InspectingPresenter {
    store: JsonlSessionStore,
    presentations: Arc<AtomicUsize>,
    stale_receipt_source: Option<PreEgressDisclosure>,
}

#[async_trait]
impl EgressDisclosurePresenter for InspectingPresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        let records = self
            .store
            .read_event_records_writer()
            .expect("presenter can inspect durable ordering");
        match disclosure.kind() {
            EgressDisclosureKind::Transport => {
                assert!(has_event(
                    &records,
                    DurableEventType::McpTransportAuthorization
                ));
                assert!(!has_event(
                    &records,
                    DurableEventType::EgressDisclosurePresented
                ));
            }
            EgressDisclosureKind::Query => {
                assert!(!has_event(&records, DurableEventType::QueryEgressStarted));
            }
        }
        self.presentations.fetch_add(1, Ordering::SeqCst);
        self.stale_receipt_source
            .as_ref()
            .unwrap_or(&disclosure)
            .presentation_receipt("deterministic-fake-sink-v1")
    }
}

fn has_event(records: &[SessionStreamRecord], event_type: DurableEventType) -> bool {
    records.iter().any(|record| {
        matches!(record, SessionStreamRecord::Stored(event) if event.event_kind() == Some(event_type))
    })
}

fn durable_runtime() -> (
    tempfile::TempDir,
    JsonlSessionStore,
    sigil_kernel::EgressAuditRecorder,
) {
    let temp = tempdir().expect("temp dir");
    let store = JsonlSessionStore::new(temp.path().join("session.jsonl")).expect("store");
    let session = Session::new("provider", "model").with_store(store.clone());
    let recorder = session.egress_audit_recorder().expect("recorder");
    (temp, store, recorder)
}

fn transport_disclosure(disclosure_id: &str) -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Transport,
        None,
        disclosure_id,
        "tui",
        "Remote MCP connection",
        "route-fingerprint",
        "profile-fingerprint",
        "https://example.com/",
        "https://example.com/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::ConnectionMetadata],
    )
    .expect("transport disclosure")
}

fn transport_authorization(disclosure_id: &str) -> McpTransportAuthorization {
    McpTransportAuthorization {
        record_id: "transport-authorization-record".to_owned(),
        root_run_id: "root-run".to_owned(),
        authorization_id: "transport-authorization".to_owned(),
        disclosure_id: disclosure_id.to_owned(),
        binding_origin: EgressBindingOrigin::UserConfigured,
        route_fingerprint: "route-fingerprint".to_owned(),
        profile_config_proxy_fingerprint: "profile-fingerprint".to_owned(),
        route: EgressNetworkRoute::Direct,
        safe_logical_destination: "https://example.com/".to_owned(),
        safe_transport_destination: "https://example.com/".to_owned(),
    }
}

fn query_disclosure(disclosure_id: &str) -> PreEgressDisclosure {
    PreEgressDisclosure::new(
        EgressDisclosureKind::Query,
        Some("query-correlation".to_owned()),
        disclosure_id,
        "tui",
        "Web search",
        "route-fingerprint",
        "profile-fingerprint",
        "https://example.com/",
        "https://example.com/",
        EgressNetworkRoute::Direct,
        vec![EgressDataCategory::SearchQuery],
    )
    .expect("query disclosure")
}

fn query_started() -> QueryEgressStarted {
    QueryEgressStarted {
        record_id: "query-start-record".to_owned(),
        root_run_id: "root-run".to_owned(),
        correlation_id: "query-correlation".to_owned(),
        route_lease_id: "route-lease".to_owned(),
        route_fingerprint: "route-fingerprint".to_owned(),
        query_chars: 5,
        query_bytes: 5,
        egress_class: WebQueryEgressClass::UserProvided,
    }
}

fn budget() -> Arc<WebTaskTreeBudget> {
    WebTaskTreeBudget::new(
        "root-run",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 2,
            max_client_search_calls: 2,
            max_hosted_requests: 2,
            max_network_attempts: 3,
            max_wire_bytes: 64,
            max_decoded_bytes: 64,
            max_model_bytes: 64,
            max_concurrent_requests: 2,
            max_attempts_per_host: 2,
        },
        None,
    )
    .expect("budget")
}

fn query_reservation(budget: &Arc<WebTaskTreeBudget>) -> sigil_kernel::WebBudgetReservation {
    budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "query-correlation".to_owned(),
            attempt_id: "query-attempt".to_owned(),
            route_lease_id: "route-lease".to_owned(),
            route_fingerprint: "route-fingerprint".to_owned(),
            kind: WebBudgetReservationKind::ClientSearchCall,
        })
        .expect("reservation")
}

fn transport_reservation(budget: &Arc<WebTaskTreeBudget>) -> sigil_kernel::WebBudgetReservation {
    budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "transport-correlation".to_owned(),
            attempt_id: "transport-attempt".to_owned(),
            route_lease_id: "transport-route-lease".to_owned(),
            route_fingerprint: "route-fingerprint".to_owned(),
            kind: WebBudgetReservationKind::TransportLifecycle,
        })
        .expect("transport reservation")
}

fn hosted_reservation(budget: &Arc<WebTaskTreeBudget>) -> sigil_kernel::WebBudgetReservation {
    budget
        .reserve(WebBudgetReservationRequest {
            correlation_id: "hosted-correlation".to_owned(),
            attempt_id: "hosted-attempt".to_owned(),
            route_lease_id: "hosted-route-lease".to_owned(),
            route_fingerprint: "hosted-fingerprint".to_owned(),
            kind: WebBudgetReservationKind::HostedProviderRequest,
        })
        .expect("hosted reservation")
}

#[tokio::test]
async fn transport_barrier_orders_durable_authorization_and_presentation_before_dns() {
    let (_temp, store, recorder) = durable_runtime();
    let presentations = Arc::new(AtomicUsize::new(0));
    let coordinator = EgressOrderingCoordinator::new(
        recorder,
        Some(Arc::new(InspectingPresenter {
            store: store.clone(),
            presentations: Arc::clone(&presentations),
            stale_receipt_source: None,
        })),
    );
    let budget = budget();
    let permit = coordinator
        .authorize_transport(
            transport_authorization("transport-disclosure"),
            transport_disclosure("transport-disclosure"),
            transport_reservation(&budget),
            &|| true,
        )
        .await
        .expect("transport permit");
    assert_eq!(presentations.load(Ordering::SeqCst), 1);

    let dns_calls = AtomicUsize::new(0);
    let _reservation = permit
        .begin_attempt("transport-attempt", "example.com")
        .expect("attempt permit");
    dns_calls.fetch_add(1, Ordering::SeqCst);
    assert_eq!(dns_calls.load(Ordering::SeqCst), 1);
    assert_eq!(budget.snapshot().expect("snapshot").network_attempts, 1);

    let order: Vec<_> = store
        .read_event_records_writer()
        .expect("records")
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event) => event.event_kind(),
            SessionStreamRecord::Legacy { .. } => None,
        })
        .collect();
    let authorization = order
        .iter()
        .position(|kind| *kind == DurableEventType::McpTransportAuthorization)
        .expect("authorization event");
    let presented = order
        .iter()
        .position(|kind| *kind == DurableEventType::EgressDisclosurePresented)
        .expect("presented event");
    assert!(authorization < presented);
}

#[tokio::test]
async fn query_barrier_orders_presentation_and_start_before_body_and_charges_budget() {
    let (_temp, store, recorder) = durable_runtime();
    let presentations = Arc::new(AtomicUsize::new(0));
    let coordinator = EgressOrderingCoordinator::new(
        recorder,
        Some(Arc::new(InspectingPresenter {
            store: store.clone(),
            presentations,
            stale_receipt_source: None,
        })),
    );
    let budget = budget();
    let authorized = coordinator
        .authorize_query(
            query_disclosure("query-disclosure"),
            query_started(),
            query_reservation(&budget),
            &|| true,
        )
        .await
        .expect("query permit");
    assert_eq!(budget.snapshot().expect("snapshot").logical_calls, 0);

    let body_bytes = AtomicUsize::new(0);
    let mut active = authorized.begin_body().expect("begin body");
    body_bytes.fetch_add(5, Ordering::SeqCst);
    active
        .charge_chunk(WebBudgetByteKind::Wire, 5)
        .expect("charge wire");
    active
        .finish(QueryEgressTerminalStatus::Completed, None)
        .expect("terminal outcome");
    assert_eq!(body_bytes.load(Ordering::SeqCst), 5);
    let snapshot = budget.snapshot().expect("snapshot");
    assert_eq!(snapshot.logical_calls, 1);
    assert_eq!(snapshot.wire_bytes, 5);

    let order: Vec<_> = store
        .read_event_records_writer()
        .expect("records")
        .into_iter()
        .filter_map(|record| match record {
            SessionStreamRecord::Stored(event) => event.event_kind(),
            SessionStreamRecord::Legacy { .. } => None,
        })
        .collect();
    let presented = order
        .iter()
        .position(|kind| *kind == DurableEventType::EgressDisclosurePresented)
        .expect("presented");
    let started = order
        .iter()
        .position(|kind| *kind == DurableEventType::QueryEgressStarted)
        .expect("started");
    let outcome = order
        .iter()
        .position(|kind| *kind == DurableEventType::QueryEgressOutcome)
        .expect("outcome");
    assert!(presented < started && started < outcome);
}

#[tokio::test]
async fn missing_or_stale_presenter_receipt_produces_zero_dns_and_query_bytes() {
    let (_temp, _store, recorder) = durable_runtime();
    let without_presenter = EgressOrderingCoordinator::new(recorder, None);
    let dns_calls = AtomicUsize::new(0);
    let transport_budget = budget();
    let result = without_presenter
        .authorize_transport(
            transport_authorization("transport-disclosure"),
            transport_disclosure("transport-disclosure"),
            transport_reservation(&transport_budget),
            &|| true,
        )
        .await;
    assert!(matches!(result, Err(EgressOrderingError::MissingPresenter)));
    assert_eq!(dns_calls.load(Ordering::SeqCst), 0);

    let (_temp, store, recorder) = durable_runtime();
    let stale = query_disclosure("old-query-disclosure");
    let coordinator = EgressOrderingCoordinator::new(
        recorder,
        Some(Arc::new(InspectingPresenter {
            store: store.clone(),
            presentations: Arc::new(AtomicUsize::new(0)),
            stale_receipt_source: Some(stale),
        })),
    );
    let budget = budget();
    let body_bytes = AtomicUsize::new(0);
    let result = coordinator
        .authorize_query(
            query_disclosure("new-query-disclosure"),
            query_started(),
            query_reservation(&budget),
            &|| true,
        )
        .await;
    assert!(matches!(result, Err(EgressOrderingError::Audit(_))));
    assert_eq!(body_bytes.load(Ordering::SeqCst), 0);
    let records = store.read_event_records_writer().expect("records");
    assert!(!has_event(&records, DurableEventType::QueryEgressStarted));
}

#[tokio::test]
async fn revocation_after_query_start_appends_cancelled_and_emits_zero_body_bytes() {
    let (_temp, store, recorder) = durable_runtime();
    let coordinator = EgressOrderingCoordinator::new(
        recorder,
        Some(Arc::new(InspectingPresenter {
            store: store.clone(),
            presentations: Arc::new(AtomicUsize::new(0)),
            stale_receipt_source: None,
        })),
    );
    let budget = budget();
    let checks = AtomicUsize::new(0);
    let admission = || checks.fetch_add(1, Ordering::SeqCst) < 3;
    let body_bytes = AtomicUsize::new(0);
    let result = coordinator
        .authorize_query(
            query_disclosure("query-disclosure"),
            query_started(),
            query_reservation(&budget),
            &admission,
        )
        .await;
    assert!(matches!(result, Err(EgressOrderingError::AdmissionRevoked)));
    assert_eq!(body_bytes.load(Ordering::SeqCst), 0);
    assert_eq!(budget.snapshot().expect("snapshot").logical_calls, 0);
    let records = store.read_event_records_writer().expect("records");
    assert!(has_event(&records, DurableEventType::QueryEgressStarted));
    assert!(has_event(&records, DurableEventType::QueryEgressOutcome));
}

#[tokio::test]
async fn post_start_budget_exhaustion_appends_failed_terminal_immediately() {
    let (_temp, store, recorder) = durable_runtime();
    let coordinator = EgressOrderingCoordinator::new(
        recorder,
        Some(Arc::new(InspectingPresenter {
            store: store.clone(),
            presentations: Arc::new(AtomicUsize::new(0)),
            stale_receipt_source: None,
        })),
    );
    let budget = budget();
    let mut active = coordinator
        .authorize_query(
            query_disclosure("query-disclosure"),
            query_started(),
            query_reservation(&budget),
            &|| true,
        )
        .await
        .expect("query authorization")
        .begin_body()
        .expect("begin body");
    assert!(matches!(
        active.charge_chunk(WebBudgetByteKind::Wire, 65),
        Err(EgressOrderingError::Budget(WebBudgetError::Exhausted {
            dimension: "wire_bytes"
        }))
    ));
    let records = store.read_event_records_writer().expect("records");
    assert!(has_event(&records, DurableEventType::QueryEgressOutcome));
    assert!(budget.snapshot().expect("snapshot").exhausted);
}

#[test]
fn hosted_authorization_is_durable_before_provider_request_permit() {
    let (_temp, store, recorder) = durable_runtime();
    let coordinator = EgressOrderingCoordinator::new(recorder, None);
    let authorization = HostedToolAuthorization {
        record_id: "hosted-authorization-record".to_owned(),
        root_run_id: "root-run".to_owned(),
        correlation_id: "hosted-correlation".to_owned(),
        authorization_id: "hosted-authorization".to_owned(),
        route_lease_id: "hosted-route-lease".to_owned(),
        hosted_request_fingerprint: "hosted-fingerprint".to_owned(),
        provider_name: "gemini".to_owned(),
        model_name: "gemini-test".to_owned(),
        effect: ApprovalMode::Allow,
        scope: HostedAuthorizationScope::ProviderRequest,
    };
    let budget = budget();
    coordinator
        .authorize_hosted_request(&authorization, hosted_reservation(&budget), &|| true)
        .expect("hosted authorization")
        .begin_request()
        .expect("begin hosted request")
        .finish(HostedToolTerminalStatus::NotUsed)
        .expect("hosted terminal");
    assert_eq!(budget.snapshot().expect("snapshot").hosted_requests, 1);
    assert!(has_event(
        &store.read_event_records_writer().expect("records"),
        DurableEventType::HostedToolAuthorization
    ));
    assert!(has_event(
        &store.read_event_records_writer().expect("records"),
        DurableEventType::HostedToolOutcome
    ));
}
