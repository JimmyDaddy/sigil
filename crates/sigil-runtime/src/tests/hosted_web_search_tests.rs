use anyhow::Result;
use serde_json::json;
use sigil_kernel::{
    HostedCustomToolCompatibility, HostedToolSupport, HostedWebSearchCapability, RootConfig,
    WebSearchRoute,
};

use super::{
    provider_hosted_route_enabled, provider_hosted_safe_destination, safe_provider_origin,
};

#[test]
fn safe_provider_origin_strips_request_paths_and_default_ports() -> Result<()> {
    assert_eq!(
        safe_provider_origin("https://Gemini.Example:443/v1beta")?,
        "https://gemini.example/"
    );
    assert_eq!(
        safe_provider_origin("http://[::1]:4317/provider/v1")?,
        "http://[::1]:4317/"
    );
    Ok(())
}

#[test]
fn safe_provider_origin_rejects_credentials_and_non_http_schemes() {
    assert!(safe_provider_origin("https://token@example.com/v1").is_err());
    assert!(safe_provider_origin("file:///tmp/provider").is_err());
}

#[test]
fn hosted_destination_uses_the_resolved_provider_base_url() -> Result<()> {
    let config: RootConfig = serde_json::from_value(json!({
        "agent": {
            "provider": "gemini",
            "model": "gemini-2.5-pro",
            "tool_timeout_secs": 30
        },
        "providers": {
            "gemini": {
                "base_url": "http://127.0.0.1:4317/gemini/v1",
                "api_key": "fixture"
            }
        }
    }))?;
    assert_eq!(
        provider_hosted_safe_destination(&config, "gemini")?,
        "http://127.0.0.1:4317/"
    );
    Ok(())
}

#[test]
fn hosted_route_falls_back_in_auto_and_rejects_forced_incompatible_composition() -> Result<()> {
    let incompatible = HostedWebSearchCapability {
        support: HostedToolSupport::ServerManaged,
        ..HostedWebSearchCapability::default()
    };
    assert!(!provider_hosted_route_enabled(
        WebSearchRoute::Auto,
        incompatible,
        "gemini"
    )?);
    assert!(
        provider_hosted_route_enabled(WebSearchRoute::ProviderHosted, incompatible, "gemini")
            .is_err()
    );

    let compatible = HostedWebSearchCapability {
        support: HostedToolSupport::ServerManaged,
        custom_tool_compatibility: HostedCustomToolCompatibility::Supported,
        ..HostedWebSearchCapability::default()
    };
    assert!(provider_hosted_route_enabled(
        WebSearchRoute::Auto,
        compatible,
        "anthropic"
    )?);
    Ok(())
}
