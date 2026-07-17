use super::*;

#[test]
fn oauth_destination_validation_is_https_exact_and_secret_free() {
    let accepted = validate_oauth_destination("https://auth.example/token?tenant=public")
        .expect("valid destination");
    assert_eq!(accepted.host_str(), Some("auth.example"));
    for rejected in [
        "http://auth.example/token",
        "https://user:secret@auth.example/token",
        "https://auth.example/token#secret",
        "file:///tmp/token",
        "not a URL",
    ] {
        assert!(matches!(
            validate_oauth_destination(rejected),
            Err(McpOAuthTransportError::DestinationRejected)
        ));
    }
}

#[test]
fn every_protocol_purpose_has_a_stable_safe_transport_label() {
    let cases = [
        (
            McpOAuthHttpPurpose::ProtectedResourceMetadata,
            "resource-discovery",
        ),
        (
            McpOAuthHttpPurpose::AuthorizationServerMetadata,
            "issuer-discovery",
        ),
        (
            McpOAuthHttpPurpose::DynamicClientRegistration,
            "client-registration",
        ),
        (McpOAuthHttpPurpose::TokenExchange, "token-exchange"),
        (McpOAuthHttpPurpose::TokenRefresh, "token-refresh"),
        (McpOAuthHttpPurpose::TokenRevocation, "token-revocation"),
    ];
    for (purpose, expected) in cases {
        assert_eq!(purpose_label(purpose), expected);
        assert!(
            expected
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'-')
        );
    }
}
