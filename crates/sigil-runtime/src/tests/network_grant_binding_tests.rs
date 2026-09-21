use crate::ProxyEnvironment;
use sigil_kernel::SecretString;
use url::Url;

#[test]
fn network_route_binding_changes_with_selected_proxy_and_bypass() {
    let endpoint = Url::parse("https://example.test/path").expect("URL");
    let proxy = |value: &str, bypass| {
        ProxyEnvironment::from_values(None, Some(SecretString::new(value)), None, bypass)
    };
    let first = proxy("http://proxy.test:8080", None).route_fingerprint(&endpoint);
    let changed = proxy("http://other-proxy.test:8080", None).route_fingerprint(&endpoint);
    let bypassed =
        proxy("http://proxy.test:8080", Some("example.test")).route_fingerprint(&endpoint);
    assert_ne!(first, changed);
    assert_ne!(first, bypassed);
    assert_eq!(
        bypassed,
        ProxyEnvironment::default().route_fingerprint(&endpoint)
    );
    assert_ne!(
        first,
        proxy("http://new-user:password@proxy.test:8080", None).route_fingerprint(&endpoint)
    );
}
