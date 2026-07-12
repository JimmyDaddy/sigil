use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use sigil_kernel::{
    DisclosurePresentationError, DisclosurePresentationReceipt, EgressBindingOrigin,
    EgressDataCategory, EgressDisclosureKind, EgressDisclosurePresenter, EgressNetworkRoute,
    JsonlSessionStore, McpTransportAuthorization, PreEgressDisclosure, SecretString, Session,
    WebBudgetReservationKind, WebBudgetReservationRequest, WebTaskTreeBudget,
    WebTaskTreeBudgetLimits,
};
use sigil_mcp::{
    McpStreamableHttpDestinationAuthorizer, McpStreamableHttpDestinationError,
    McpStreamableHttpHeaderConfig, McpStreamableHttpHeaderEnvironment,
    McpStreamableHttpRouteEvidence, PreparedMcpStreamableHttpHeaders,
};
use tempfile::tempdir;
use url::Url;

use crate::{
    EgressOrderingCoordinator, IpCidr, ProxyEnvironment,
    QueuedRuntimeMcpStreamableHttpAttemptFactory, RuntimeMcpStreamableHttpAttempt,
    RuntimeMcpStreamableHttpAttemptFactory, RuntimeMcpStreamableHttpDestinationAuthorizer,
    RuntimeMcpTransportAttemptFactory, WebDestinationError, WebDestinationGuard,
    WebDestinationGuardPolicy, WebDestinationResolver,
};

#[tokio::test]
async fn production_attempt_factory_issues_unique_disclosures_and_shared_budget_reservations() {
    let budget = WebTaskTreeBudget::new(
        "remote-root",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 2,
            max_client_search_calls: 2,
            max_hosted_requests: 1,
            max_network_attempts: 4,
            max_wire_bytes: 1024,
            max_decoded_bytes: 1024,
            max_model_bytes: 1024,
            max_concurrent_requests: 2,
            max_attempts_per_host: 4,
        },
        None,
    )
    .expect("budget");
    let factory = RuntimeMcpTransportAttemptFactory::new(
        budget,
        "remote-root",
        sigil_kernel::EgressBindingOrigin::UserConfigured,
        "remote-disclosure",
        "tui",
        "Remote MCP",
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        "https://example.test/",
        "https://example.test/",
        sigil_kernel::EgressNetworkRoute::Direct,
        vec![sigil_kernel::EgressDataCategory::ConnectionMetadata],
    );
    let first = factory.next_attempt().await.expect("first attempt");
    let second = factory.next_attempt().await.expect("second attempt");
    assert_ne!(
        first.authorization.disclosure_id,
        second.authorization.disclosure_id
    );
    assert_ne!(first.attempt_id, second.attempt_id);

    let next_budget = WebTaskTreeBudget::new(
        "next-remote-root",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 2,
            max_client_search_calls: 2,
            max_hosted_requests: 1,
            max_network_attempts: 4,
            max_wire_bytes: 1024,
            max_decoded_bytes: 1024,
            max_model_bytes: 1024,
            max_concurrent_requests: 2,
            max_attempts_per_host: 4,
        },
        None,
    )
    .expect("next budget");
    factory
        .rebind_budget(next_budget)
        .expect("rebind current run budget");
    let rebound = factory.next_attempt().await.expect("rebound attempt");
    assert_eq!(rebound.authorization.root_run_id, "next-remote-root");
}

#[derive(Clone)]
struct SequenceResolver {
    calls: Arc<AtomicUsize>,
    answers: Arc<Mutex<VecDeque<Vec<IpAddr>>>>,
}

#[async_trait]
impl WebDestinationResolver for SequenceResolver {
    async fn resolve_all(
        &self,
        _host: &str,
        _port: u16,
    ) -> Result<Vec<IpAddr>, WebDestinationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answers
            .lock()
            .map_err(|_| WebDestinationError::ResolutionFailed)?
            .pop_front()
            .ok_or(WebDestinationError::ResolutionFailed)
    }
}

struct ReceiptPresenter;

struct EmptyHeaderEnvironment;

impl McpStreamableHttpHeaderEnvironment for EmptyHeaderEnvironment {
    fn resolve(&self, _name: &str) -> Option<SecretString> {
        None
    }
}

#[async_trait]
impl EgressDisclosurePresenter for ReceiptPresenter {
    async fn present(
        &self,
        disclosure: PreEgressDisclosure,
    ) -> Result<DisclosurePresentationReceipt, DisclosurePresentationError> {
        disclosure.presentation_receipt("runtime-streamable-http-fixture-v1")
    }
}

fn fixture(
    presenter: Option<Arc<dyn EgressDisclosurePresenter>>,
    policy: WebDestinationGuardPolicy,
    proxy: ProxyEnvironment,
    answers: Vec<Vec<IpAddr>>,
    attempt_count: usize,
) -> (
    RuntimeMcpStreamableHttpDestinationAuthorizer<SequenceResolver>,
    Arc<AtomicUsize>,
) {
    let temp = tempdir().expect("temp");
    let durable_dir = temp.keep();
    let store = JsonlSessionStore::new(durable_dir.join("session.jsonl")).expect("store");
    let recorder = Session::new("provider", "model")
        .with_store(store)
        .egress_audit_recorder()
        .expect("recorder");
    let budget = WebTaskTreeBudget::new(
        "root-run",
        WebTaskTreeBudgetLimits {
            max_fetch_calls: 8,
            max_client_search_calls: 8,
            max_hosted_requests: 8,
            max_network_attempts: 16,
            max_wire_bytes: 1024 * 1024,
            max_decoded_bytes: 1024 * 1024,
            max_model_bytes: 1024 * 1024,
            max_concurrent_requests: 8,
            max_attempts_per_host: 16,
        },
        None,
    )
    .expect("budget");
    let calls = Arc::new(AtomicUsize::new(0));
    let guard = Arc::new(WebDestinationGuard::new(
        SequenceResolver {
            calls: Arc::clone(&calls),
            answers: Arc::new(Mutex::new(answers.into())),
        },
        policy,
        proxy,
    ));
    let endpoint = SecretString::new("https://example.test/mcp");
    let prepared = PreparedMcpStreamableHttpHeaders::prepare(
        endpoint.clone(),
        &McpStreamableHttpHeaderConfig::default(),
        &EmptyHeaderEnvironment,
    )
    .expect("prepared headers");
    let live_header_fingerprint = prepared.live_header_fingerprint().to_owned();
    let preview = guard
        .preview(Url::parse(endpoint.expose_secret()).expect("endpoint"))
        .expect("preview");
    let route = if preview.safe_logical_destination() == preview.safe_transport_destination() {
        EgressNetworkRoute::Direct
    } else {
        EgressNetworkRoute::ProxyRemote
    };
    let attempts = (0..attempt_count)
        .map(|index| {
            let correlation = format!("transport-correlation-{index}");
            let attempt_id = format!("transport-attempt-{index}");
            let disclosure_id = format!("disclosure-{index}");
            let reservation = budget
                .reserve(WebBudgetReservationRequest {
                    correlation_id: correlation,
                    attempt_id: attempt_id.clone(),
                    route_lease_id: format!("transport-lease-{index}"),
                    route_fingerprint: "route-fingerprint".to_owned(),
                    kind: WebBudgetReservationKind::TransportLifecycle,
                })
                .expect("reservation");
            let disclosure = PreEgressDisclosure::new(
                EgressDisclosureKind::Transport,
                None,
                disclosure_id.clone(),
                "tui",
                "Remote MCP connection",
                "route-fingerprint",
                "profile-fingerprint",
                preview.safe_logical_destination(),
                preview.safe_transport_destination(),
                route,
                vec![EgressDataCategory::ConnectionMetadata],
            )
            .expect("disclosure");
            let authorization = McpTransportAuthorization {
                record_id: format!("record-{index}"),
                root_run_id: "root-run".to_owned(),
                authorization_id: format!("authorization-{index}"),
                disclosure_id,
                binding_origin: EgressBindingOrigin::UserConfigured,
                route_fingerprint: "route-fingerprint".to_owned(),
                profile_config_proxy_fingerprint: "profile-fingerprint".to_owned(),
                route,
                safe_logical_destination: preview.safe_logical_destination().to_owned(),
                safe_transport_destination: preview.safe_transport_destination().to_owned(),
            };
            RuntimeMcpStreamableHttpAttempt::new(authorization, disclosure, reservation, attempt_id)
        })
        .collect::<Vec<_>>();
    (
        RuntimeMcpStreamableHttpDestinationAuthorizer::new(
            endpoint,
            guard,
            EgressOrderingCoordinator::new(recorder, presenter),
            Arc::new(QueuedRuntimeMcpStreamableHttpAttemptFactory::new(attempts)),
            "profile-fingerprint",
            live_header_fingerprint,
            Arc::new(|| true),
        ),
        calls,
    )
}

#[tokio::test]
async fn streamable_http_runtime_consumes_public_prepared_header_fingerprint() {
    let prepared = PreparedMcpStreamableHttpHeaders::prepare(
        SecretString::new("https://example.test/mcp"),
        &McpStreamableHttpHeaderConfig::default(),
        &EmptyHeaderEnvironment,
    )
    .expect("cross-crate header preflight");
    let expected = prepared.live_header_fingerprint().to_owned();
    let (authorizer, _) = fixture(
        Some(Arc::new(ReceiptPresenter)),
        WebDestinationGuardPolicy::default(),
        ProxyEnvironment::default(),
        vec![vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))]],
        1,
    );
    assert_eq!(authorizer.live_header_fingerprint(), expected);
    authorizer
        .authorize_destination()
        .await
        .expect("prepared fingerprint bound runtime plan");
}

#[tokio::test]
async fn streamable_http_runtime_pre_egress_failure_has_zero_resolver_calls() {
    let (authorizer, calls) = fixture(
        None,
        WebDestinationGuardPolicy::default(),
        ProxyEnvironment::default(),
        vec![vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))]],
        1,
    );
    let error = authorizer
        .authorize_destination()
        .await
        .expect_err("missing presenter must fail");
    assert_eq!(error, McpStreamableHttpDestinationError::PreEgressRejected);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn streamable_http_runtime_direct_guard_pins_complete_a_and_aaaa_set() {
    let (authorizer, calls) = fixture(
        Some(Arc::new(ReceiptPresenter)),
        WebDestinationGuardPolicy::default(),
        ProxyEnvironment::default(),
        vec![vec![
            IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34)),
            IpAddr::V6("2606:2800:220:1:248:1893:25c8:1946".parse().expect("IPv6")),
        ]],
        1,
    );
    let plan = authorizer
        .authorize_destination()
        .await
        .expect("authorized");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        plan.evidence(),
        McpStreamableHttpRouteEvidence::DirectAllAddressesPinned
    );
}

#[tokio::test]
async fn streamable_http_runtime_proxy_guards_logical_destination_without_dns_claim() {
    let proxy = ProxyEnvironment::from_values(
        None,
        Some(SecretString::new("http://proxy.example:8080")),
        None,
        None,
    );
    let (authorizer, calls) = fixture(
        Some(Arc::new(ReceiptPresenter)),
        WebDestinationGuardPolicy::default(),
        proxy,
        Vec::new(),
        1,
    );
    let plan = authorizer
        .authorize_destination()
        .await
        .expect("proxy plan");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        plan.evidence(),
        McpStreamableHttpRouteEvidence::ProxyRemoteLogicalGuardOnly
    );
    assert_eq!(
        plan.safe_transport_destination(),
        "http://proxy.example:8080/"
    );
}

#[tokio::test]
async fn streamable_http_runtime_private_exception_requires_exact_host_and_all_cidr_addresses() {
    let policy = WebDestinationGuardPolicy::default().with_private_exception(
        "example.test",
        vec![IpCidr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)), 8).expect("cidr")],
    );
    let (accepted, _) = fixture(
        Some(Arc::new(ReceiptPresenter)),
        policy.clone(),
        ProxyEnvironment::default(),
        vec![vec![IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))]],
        1,
    );
    accepted
        .authorize_destination()
        .await
        .expect("exact private grant");

    let (mixed, _) = fixture(
        Some(Arc::new(ReceiptPresenter)),
        policy,
        ProxyEnvironment::default(),
        vec![vec![
            IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)),
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2)),
        ]],
        1,
    );
    assert!(matches!(
        mixed.authorize_destination().await,
        Err(McpStreamableHttpDestinationError::DestinationRejected)
    ));
}

#[tokio::test]
async fn streamable_http_runtime_metadata_and_loopback_are_never_overridden() {
    let policy = WebDestinationGuardPolicy::default().with_private_exception(
        "example.test",
        vec![
            IpCidr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0).expect("all IPv4"),
            IpCidr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0).expect("all IPv6"),
        ],
    );
    for address in [
        IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ] {
        let (authorizer, _) = fixture(
            Some(Arc::new(ReceiptPresenter)),
            policy.clone(),
            ProxyEnvironment::default(),
            vec![vec![address]],
            1,
        );
        assert!(matches!(
            authorizer.authorize_destination().await,
            Err(McpStreamableHttpDestinationError::DestinationRejected)
        ));
    }
}

#[tokio::test]
async fn streamable_http_runtime_reauthorizes_and_rejects_public_to_private_rebinding() {
    let (authorizer, calls) = fixture(
        Some(Arc::new(ReceiptPresenter)),
        WebDestinationGuardPolicy::default(),
        ProxyEnvironment::default(),
        vec![
            vec![IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))],
            vec![IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3))],
        ],
        2,
    );
    authorizer
        .authorize_destination()
        .await
        .expect("first public attempt");
    assert!(matches!(
        authorizer.authorize_destination().await,
        Err(McpStreamableHttpDestinationError::DestinationRejected)
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
