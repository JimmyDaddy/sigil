use sha2::{Digest, Sha256};
use sigil_kernel::SecretRedactor;
use sigil_mcp::McpSearchAdapterKind;

use super::*;

fn hash(label: &str) -> String {
    format!("{:x}", Sha256::digest(label.as_bytes()))
}

fn pending(epoch: u64) -> PendingMcpSearchBinding {
    PendingMcpSearchBinding {
        server_name: "user-search".to_owned(),
        tool_name: "search".to_owned(),
        origin: McpSearchBindingOrigin::UserConfigured,
        root_run_id: "root-run".to_owned(),
        config_epoch: epoch,
    }
}

fn prepared(epoch: u64, schema: &str) -> PreparedMcpSearchBinding {
    PreparedMcpSearchBinding {
        server_name: "user-search".to_owned(),
        tool_name: "search".to_owned(),
        origin: McpSearchBindingOrigin::UserConfigured,
        adapter: McpSearchAdapterKind::GenericQueryText,
        safe_destination: "https://search.example:443".to_owned(),
        server_identity_fingerprint: hash("identity"),
        tool_schema_fingerprint: hash(schema),
        transport_fingerprint: hash("transport"),
        live_header_fingerprint: format!("hmac-sha256:{}", hash("headers")),
        source_policy_fingerprint: hash("source"),
        effective_policy_fingerprint: hash("policy"),
        profile_config_proxy_fingerprint: hash("profile"),
        root_run_id: "root-run".to_owned(),
        config_epoch: epoch,
    }
}

#[test]
fn configured_binding_is_authoritative_in_all_present_states() -> anyhow::Result<()> {
    let registry = McpSearchBindingRegistry::default();
    assert_eq!(
        registry.select_auto(true)?,
        StableMcpRouteSelection::Bundled
    );
    let revision = registry.declare(pending(1))?;
    assert_eq!(
        registry.select_auto(true)?,
        StableMcpRouteSelection::ConfiguredPending
    );
    registry.activate(
        revision,
        Err(WebSearchFailure::new(WebSearchFailureClass::SchemaDrift)),
    )?;
    assert!(matches!(
        registry.select_auto(true)?,
        StableMcpRouteSelection::ConfiguredUnavailable(_)
    ));
    registry.clear()?;
    assert_eq!(
        registry.select_auto(true)?,
        StableMcpRouteSelection::Bundled
    );
    Ok(())
}

#[test]
fn atomic_rebind_invalidates_old_prepared_lease() -> anyhow::Result<()> {
    let registry = McpSearchBindingRegistry::default();
    let first = registry.declare(pending(1))?;
    registry.activate(first, Ok(prepared(1, "schema-one")))?;
    let StableMcpRouteSelection::Configured(old_lease) = registry.select_auto(true)? else {
        anyhow::bail!("expected first lease");
    };
    let second = registry.declare(pending(2))?;
    assert_eq!(
        registry.validate_lease(&old_lease),
        Err(McpSearchBindingRegistryError::StaleLease)
    );
    registry.activate(second, Ok(prepared(2, "schema-two")))?;
    assert_eq!(
        registry.validate_lease(&old_lease),
        Err(McpSearchBindingRegistryError::StaleLease)
    );
    Ok(())
}

#[test]
fn query_normalization_is_nfc_control_safe_and_route_specific() -> anyhow::Result<()> {
    let redactor = SecretRedactor::from_values(["known-secret"]);
    let normalized =
        normalize_web_search_query("  Cafe\u{301}\u{1b}[31m\n docs  ", &redactor, false)?;
    assert_eq!(normalized.query.expose_secret(), "Café docs");
    assert_eq!(normalized.chars, 9);
    assert_eq!(
        generic_query_arguments(&normalized.query),
        json!({"query":"Café docs"})
    );
    for (query, expected) in [
        ("known-secret", WebSearchFailureClass::SecretBlocked),
        (
            "person@example.com",
            WebSearchFailureClass::SensitivePersonalDataBlocked,
        ),
        (
            "+14155552671",
            WebSearchFailureClass::SensitivePersonalDataBlocked,
        ),
        (
            "4111-1111-1111-1111",
            WebSearchFailureClass::SensitivePersonalDataBlocked,
        ),
    ] {
        let error = normalize_web_search_query(query, &redactor, true).expect_err("blocked");
        assert!(matches!(
            error,
            WebSearchConnectorError::Failed(WebSearchFailure { class, .. }) if class == expected
        ));
    }
    assert!(normalize_web_search_query("Ada Lovelace", &redactor, true).is_ok());
    Ok(())
}

#[test]
fn failure_contract_requires_protocol_detail_exactly_for_protocol_class() {
    assert!(
        WebSearchFailure::protocol(WebSearchProtocolFailureKind::MalformedEnvelope)
            .validate()
            .is_ok()
    );
    let invalid = WebSearchFailure {
        class: WebSearchFailureClass::Timeout,
        retry_after_secs: None,
        protocol_detail: Some(WebSearchProtocolFailureKind::MalformedEnvelope),
    };
    assert!(invalid.validate().is_err());

    let mut unsafe_live_binding = prepared(1, "schema");
    unsafe_live_binding.live_header_fingerprint = hash("raw-header-material");
    assert_eq!(
        unsafe_live_binding.validate(),
        Err(McpSearchBindingRegistryError::InvalidBinding)
    );
}
