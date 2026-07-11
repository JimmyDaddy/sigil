use std::{
    collections::VecDeque,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use sigil_tools_builtin::{WebFetchNetworkGuard, WebFetchRoute, WebFetchTransportSecurity};

use super::*;

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
            .expect("resolver answers should lock")
            .pop_front()
            .ok_or(WebDestinationError::ResolutionFailed)
    }
}

#[tokio::test]
async fn direct_route_validates_all_addresses_and_returns_a_pinned_plan() {
    let resolver = FakeResolver::new([vec![ip("93.184.216.34"), ip("2606:4700:4700::1111")]]);
    let allowed_guard = guard(
        resolver.clone(),
        WebDestinationGuardPolicy::default(),
        no_proxy(),
    );
    let plan = allowed_guard
        .authorize(url("https://example.test/path?token=secret"))
        .await
        .expect("public address set should be authorized");

    assert_eq!(resolver.calls(), 1);
    assert_eq!(plan.route(), WebFetchRoute::Direct);
    assert_eq!(
        plan.network_guard(),
        WebFetchNetworkGuard::DirectAllAddressesPinned
    );
    assert_eq!(
        plan.transport_security(),
        WebFetchTransportSecurity::DirectPinned
    );
    let debug = format!("{plan:?}");
    assert!(!debug.contains("token=secret"));
    assert!(debug.contains("direct_address_count: 2"));
}

#[tokio::test]
async fn mixed_public_private_and_permanent_address_sets_are_rejected() {
    for (answer, expected_permanent) in [
        (vec![ip("93.184.216.34"), ip("10.0.0.7")], false),
        (vec![ip("93.184.216.34"), ip("127.0.0.1")], true),
        (vec![ip("2606:4700:4700::1111"), ip("fe80::1")], true),
    ] {
        let guard = guard(
            FakeResolver::new([answer]),
            WebDestinationGuardPolicy::default(),
            no_proxy(),
        );
        let error = guard
            .authorize(url("https://mixed.example.test"))
            .await
            .expect_err("mixed set should fail closed");
        if expected_permanent {
            assert!(matches!(
                error,
                WebDestinationError::PermanentlyForbiddenAddress
            ));
        } else {
            assert!(matches!(error, WebDestinationError::PrivateAddressDenied));
        }
    }
}

#[tokio::test]
async fn permanent_address_categories_cannot_be_overridden() {
    for address in [
        "0.0.0.0",
        "127.0.0.1",
        "169.254.169.254",
        "169.254.1.2",
        "224.0.0.1",
        "240.0.0.1",
        "100.64.0.1",
        "192.0.0.1",
        "198.18.0.1",
        "::",
        "::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
    ] {
        let policy = WebDestinationGuardPolicy::default().with_private_exception(
            "internal.example.test",
            vec![
                IpCidr::new(ip(address), if address.contains(':') { 128 } else { 32 })
                    .expect("CIDR should parse"),
            ],
        );
        let guard = guard(FakeResolver::new([vec![ip(address)]]), policy, no_proxy());
        let error = guard
            .authorize(url("https://internal.example.test"))
            .await
            .expect_err("permanent category cannot be overridden");
        assert!(
            matches!(error, WebDestinationError::PermanentlyForbiddenAddress),
            "unexpected result for {address}: {error:?}"
        );
    }
}

#[tokio::test]
async fn private_exception_requires_exact_host_and_every_address_in_cidr() {
    let cidr = IpCidr::new(ip("10.20.0.0"), 16).expect("CIDR should parse");
    let policy = WebDestinationGuardPolicy::default()
        .with_private_exception("internal.example.test", vec![cidr]);
    let allowed_guard = guard(
        FakeResolver::new([vec![ip("10.20.1.2"), ip("10.20.2.3")]]),
        policy.clone(),
        no_proxy(),
    );
    let plan = allowed_guard
        .authorize(url("https://internal.example.test"))
        .await
        .expect("exact host and full CIDR set should be authorized");
    assert_eq!(plan.route(), WebFetchRoute::Direct);

    let wrong_host = guard(
        FakeResolver::new([vec![ip("10.20.1.2")]]),
        policy.clone(),
        no_proxy(),
    )
    .authorize(url("https://other.example.test"))
    .await
    .expect_err("CIDR grant must not apply to another host");
    assert!(matches!(
        wrong_host,
        WebDestinationError::PrivateAddressDenied
    ));

    let outside_cidr = guard(
        FakeResolver::new([vec![ip("10.20.1.2"), ip("10.21.1.2")]]),
        policy,
        no_proxy(),
    )
    .authorize(url("https://internal.example.test"))
    .await
    .expect_err("every resolved address must be in the CIDR grant");
    assert!(matches!(
        outside_cidr,
        WebDestinationError::PrivateAddressDenied
    ));
}

#[tokio::test]
async fn ipv4_mapped_ipv6_cannot_bypass_ipv4_classification() {
    let mapped_private = IpAddr::V6(Ipv4Addr::new(10, 0, 0, 1).to_ipv6_mapped());
    let guard = guard(
        FakeResolver::new([vec![mapped_private]]),
        WebDestinationGuardPolicy::default(),
        no_proxy(),
    );
    let error = guard
        .authorize(url("https://mapped.example.test"))
        .await
        .expect_err("mapped private address should be denied");
    assert!(matches!(error, WebDestinationError::PrivateAddressDenied));
}

#[tokio::test]
async fn each_authorization_re_resolves_and_rebinding_is_rejected() {
    let resolver = FakeResolver::new([vec![ip("93.184.216.34")], vec![ip("10.0.0.1")]]);
    let guard = guard(
        resolver.clone(),
        WebDestinationGuardPolicy::default(),
        no_proxy(),
    );
    guard
        .authorize(url("https://rebind.example.test"))
        .await
        .expect("first public answer should pass");
    let error = guard
        .authorize(url("https://rebind.example.test"))
        .await
        .expect_err("second private answer should fail");
    assert!(matches!(error, WebDestinationError::PrivateAddressDenied));
    assert_eq!(resolver.calls(), 2);
}

#[tokio::test]
async fn proxy_mode_validates_logical_destination_and_reports_proxy_remote() {
    let resolver = FakeResolver::new([]);
    let proxies = ProxyEnvironment::from_values(
        None,
        Some(SecretString::new(
            "http://proxy-user:proxy-secret@proxy.example:8080",
        )),
        None,
        None,
    );
    let guard = guard(
        resolver.clone(),
        WebDestinationGuardPolicy::default(),
        proxies,
    );
    let plan = guard
        .authorize(url("https://public.example.test/path?secret=value"))
        .await
        .expect("proxy route should be authorized without local DNS");
    assert_eq!(resolver.calls(), 0);
    assert_eq!(plan.route(), WebFetchRoute::EnvironmentProxy);
    assert_eq!(
        plan.transport_security(),
        WebFetchTransportSecurity::ProxyRemote
    );
    assert_eq!(
        plan.network_guard(),
        WebFetchNetworkGuard::ProxyLogicalDestinationOnly
    );
    let debug = format!("{plan:?}");
    assert!(!debug.contains("proxy-user"));
    assert!(!debug.contains("proxy-secret"));
    assert!(!debug.contains("secret=value"));
    assert!(debug.contains("proxy.example:8080"));
}

#[tokio::test]
async fn proxy_mode_rejects_metadata_localhost_and_private_exception_without_dns() {
    for host in ["localhost", "metadata.google.internal", "169.254.169.254"] {
        let resolver = FakeResolver::new([]);
        let guard = guard(
            resolver.clone(),
            WebDestinationGuardPolicy::default(),
            https_proxy(None),
        );
        let error = guard
            .authorize(url(&format!("https://{host}")))
            .await
            .expect_err("logical destination guard should reject host");
        assert_eq!(resolver.calls(), 0);
        assert!(matches!(
            error,
            WebDestinationError::BlockedDomain | WebDestinationError::PermanentlyForbiddenAddress
        ));
    }

    let resolver = FakeResolver::new([]);
    let policy = WebDestinationGuardPolicy::default().with_private_exception(
        "internal.example.test",
        vec![IpCidr::new(ip("10.0.0.0"), 8).expect("CIDR should parse")],
    );
    let guard = guard(resolver.clone(), policy, https_proxy(None));
    let error = guard
        .authorize(url("https://internal.example.test"))
        .await
        .expect_err("proxy route has no complete resolved set receipt");
    assert_eq!(resolver.calls(), 0);
    assert!(matches!(
        error,
        WebDestinationError::PrivateExceptionRequiresResolvedSet
    ));
}

#[tokio::test]
async fn no_proxy_returns_to_direct_guard_instead_of_bypassing_it() {
    let resolver = FakeResolver::new([vec![ip("10.0.0.1")]]);
    let guard = guard(
        resolver.clone(),
        WebDestinationGuardPolicy::default(),
        https_proxy(Some(".example.test")),
    );
    let error = guard
        .authorize(url("https://private.example.test"))
        .await
        .expect_err("NO_PROXY should return to the direct private-address guard");
    assert_eq!(resolver.calls(), 1);
    assert!(matches!(error, WebDestinationError::PrivateAddressDenied));
}

#[tokio::test]
async fn invalid_url_port_and_blocked_suffix_fail_before_dns() {
    let policy = WebDestinationGuardPolicy::default()
        .with_blocked_domains(["blocked.example", ".also-blocked.example"]);
    for input in [
        "ftp://example.test",
        "https://user:password@example.test",
        "https://example.test:8443",
        "https://child.blocked.example",
        "https://also-blocked.example",
    ] {
        let resolver = FakeResolver::new([]);
        let guard = guard(resolver.clone(), policy.clone(), no_proxy());
        guard
            .authorize(url(input))
            .await
            .expect_err("pre-DNS validation should reject input");
        assert_eq!(resolver.calls(), 0, "resolver called for {input}");
    }
}

fn guard(
    resolver: FakeResolver,
    policy: WebDestinationGuardPolicy,
    proxies: ProxyEnvironment,
) -> WebDestinationGuard<FakeResolver> {
    WebDestinationGuard::new(resolver, policy, proxies)
}

fn no_proxy() -> ProxyEnvironment {
    ProxyEnvironment::default()
}

fn https_proxy(no_proxy: Option<&str>) -> ProxyEnvironment {
    ProxyEnvironment::from_values(
        None,
        Some(SecretString::new("http://proxy.example:8080")),
        None,
        no_proxy,
    )
}

fn url(value: &str) -> Url {
    Url::parse(value).expect("test URL should parse")
}

fn ip(value: &str) -> IpAddr {
    value.parse().expect("test IP address should parse")
}
