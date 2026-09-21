//! Exact, secret-free network execution identity for permission session grants.
use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::json;
use sigil_kernel::RootConfig;
use url::Url;

/// Hash the actual selected proxy, including bypass rules, rather than a generic route label.
/// Destination enforcement, DNS/SSRF checks and capability provenance still run per request.
pub(crate) fn network_grant_bindings(
    root: &RootConfig,
    endpoint: &str,
    transport: &str,
    route_identity: &serde_json::Value,
) -> Result<BTreeMap<String, String>> {
    let proxy = crate::remote_mcp::proxy_environment(root);
    let selected_route = Url::parse(endpoint)
        .ok()
        .map(|url| proxy.route_fingerprint(&url));
    let ca = std::env::var_os("SSL_CERT_FILE")
        .map(|path| std::fs::read(&path).map(|bytes| sigil_kernel::sha256_hex(&bytes)))
        .transpose()?;
    let hash = |value: &serde_json::Value| -> Result<String> {
        Ok(sigil_kernel::sha256_hex(&serde_json::to_vec(value)?))
    };
    Ok(BTreeMap::from([
        (
            "network_endpoint_hash".to_owned(),
            sigil_kernel::sha256_hex(endpoint.as_bytes()),
        ),
        (
            "network_transport_hash".to_owned(),
            hash(&json!({"transport": transport, "ca": ca}))?,
        ),
        (
            "network_route_hash".to_owned(),
            hash(&json!({"route": route_identity, "proxy": selected_route}))?,
        ),
        (
            "network_policy_hash".to_owned(),
            hash(&serde_json::to_value(&root.web)?)?,
        ),
    ]))
}

pub(crate) fn configured_search_route_identity(
    server: &sigil_kernel::McpServerConfig,
) -> Result<serde_json::Value> {
    let names: Vec<&str> = if let Some(remote) = server.streamable_http() {
        remote
            .env_http_headers
            .values()
            .map(String::as_str)
            .chain(remote.bearer_token_env_var.as_deref())
            .collect()
    } else {
        server.stdio().map_or_else(Vec::new, |(_, _, inherit)| {
            inherit.iter().map(String::as_str).collect()
        })
    };
    let environment: BTreeMap<_, _> = names
        .into_iter()
        .map(|name| {
            (
                name,
                std::env::var(name)
                    .ok()
                    .map(|value| sigil_kernel::sha256_hex(value.as_bytes())),
            )
        })
        .collect();
    // This value is only hashed into analysis bindings; credentials are never persisted.
    Ok(json!({"server": serde_json::to_value(server)?, "environment": environment}))
}

#[cfg(test)]
#[path = "tests/network_grant_binding_tests.rs"]
mod tests;
