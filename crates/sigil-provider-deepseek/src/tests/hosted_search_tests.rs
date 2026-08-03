use sigil_kernel::{
    HostedCitationFidelity, HostedConstraintEnforcement, HostedCustomToolCompatibility,
    HostedQueryVisibility, HostedSourceFidelity, HostedToolKind, HostedToolLimits,
    HostedToolRequest, HostedToolSupport,
};

use super::{
    DEEPSEEK_WEB_SEARCH_TOOL_TYPE, hosted_web_search_capability, hosted_web_search_request,
    is_hosted_web_search_model,
};

#[test]
fn capability_fails_closed_when_messages_path_disabled() {
    let capability = hosted_web_search_capability("deepseek-v4-flash", false);
    assert!(!capability.is_supported());
    assert_eq!(capability.support, HostedToolSupport::Unsupported);
}

#[test]
fn capability_fails_closed_for_unknown_models() {
    assert!(!hosted_web_search_capability("deepseek-r1", true).is_supported());
    assert!(!hosted_web_search_capability("deepseek-v4-flash-extra", true).is_supported());
    assert!(!hosted_web_search_capability("", true).is_supported());
}

#[test]
fn capability_supports_admitted_v4_models() {
    for model in ["deepseek-v4-flash", "deepseek-v4-pro"] {
        let capability = hosted_web_search_capability(model, true);
        assert!(capability.is_supported(), "{model} must be admitted");
        assert_eq!(capability.support, HostedToolSupport::ServerManaged);
        assert_eq!(
            capability.query_visibility,
            HostedQueryVisibility::ProviderReportedPostExecution
        );
        assert_eq!(
            capability.source_fidelity,
            HostedSourceFidelity::UrlAndTitle
        );
        assert_eq!(
            capability.citation_fidelity,
            HostedCitationFidelity::OutputSpan
        );
        assert_eq!(
            capability.max_uses_enforcement,
            HostedConstraintEnforcement::Hard
        );
        assert_eq!(
            capability.domain_filter_enforcement,
            HostedConstraintEnforcement::Hard
        );
        assert_eq!(
            capability.custom_tool_compatibility,
            HostedCustomToolCompatibility::Supported
        );
    }
}

#[test]
fn model_admission_is_pinned() {
    assert!(is_hosted_web_search_model("deepseek-v4-flash"));
    assert!(is_hosted_web_search_model("deepseek-v4-pro"));
    assert!(!is_hosted_web_search_model("deepseek-r1"));
    assert!(!is_hosted_web_search_model("gpt-4o"));
}

#[test]
fn wire_tool_type_matches_deepseek_anthropic_endpoint() {
    assert_eq!(DEEPSEEK_WEB_SEARCH_TOOL_TYPE, "web_search_20250305");
}

#[test]
fn empty_declaration_list_returns_none() {
    assert!(
        hosted_web_search_request(&[])
            .expect("empty list resolves")
            .is_none()
    );
}

#[test]
fn single_declaration_is_returned() {
    let request = HostedToolRequest::new(
        "auth-1",
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )
    .expect("valid request");
    let requests = [request];
    let found = hosted_web_search_request(&requests)
        .expect("single declaration resolves")
        .expect("declaration present");
    assert_eq!(found.authorization_id, "auth-1");
}

#[test]
fn multiple_declarations_are_rejected() {
    let request = |auth: &str| {
        HostedToolRequest::new(auth, HostedToolKind::WebSearch, HostedToolLimits::default())
            .expect("valid request")
    };
    let error = hosted_web_search_request(&[request("auth-1"), request("auth-2")])
        .expect_err("duplicate declarations must fail");
    assert!(error.to_string().contains("more than one"));
}

#[test]
fn corrupted_declaration_fails_validation() {
    let mut request = HostedToolRequest::new(
        "auth-1",
        HostedToolKind::WebSearch,
        HostedToolLimits::default(),
    )
    .expect("valid request");
    request.request_fingerprint = "hosted-v1:corrupted".to_owned();
    let error = hosted_web_search_request(&[request])
        .expect_err("corrupted fingerprint must fail validation");
    assert!(error.to_string().contains("fingerprint"));
}
