use super::*;

#[test]
fn error_code_projection_accepts_only_bounded_machine_labels() {
    assert_eq!(
        safe_error_code("stale_cursor".to_owned()).as_deref(),
        Some("stale_cursor")
    );
    assert!(safe_error_code("contains space".to_owned()).is_none());
    assert!(safe_error_code("x".repeat(129)).is_none());
}

#[test]
fn typed_client_debug_never_projects_transport_or_bearer_material() {
    let bearer = Arc::new(DesktopBearerToken::generate().expect("token should generate"));
    let client = DesktopHttpClient::new(
        Client::new(),
        "127.0.0.1:3210".parse().expect("address should parse"),
        bearer,
    );
    let debug = format!("{client:?}");

    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("3210"));
}
