use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use reqwest::Client;
use sigil_provider_deepseek::DeepSeekProviderConfig;
use sigil_runtime::resolve_deepseek_api_key;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BalanceSnapshot {
    pub total: Option<f64>,
    pub currency: Option<String>,
    pub available: bool,
    pub status: String,
}

#[cfg_attr(coverage, allow(dead_code))]
pub(crate) async fn fetch_remote_model_ids(config: &DeepSeekProviderConfig) -> Result<Vec<String>> {
    let (api_key, url, timeout_secs) = provider_status_request_parts(config, "models")?;
    let client = build_provider_status_client(timeout_secs, "model-list")?;
    let response = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| anyhow!("failed to fetch provider models: {error}"))?;
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| anyhow!("failed to decode provider models: {error}"))?;
    let models = parse_remote_model_ids(&payload);
    if models.is_empty() {
        bail!("provider returned no model ids");
    }
    Ok(models)
}

pub(crate) async fn fetch_provider_balance_snapshot(
    config: &DeepSeekProviderConfig,
) -> Result<BalanceSnapshot> {
    let (api_key, url, timeout_secs) = provider_status_request_parts(config, "user/balance")?;
    let client = build_provider_status_client(timeout_secs, "balance")?;
    let payload = client
        .get(url)
        .bearer_auth(api_key)
        .header("Accept", "application/json")
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| anyhow!("failed to fetch balance: {error}"))?
        .json::<serde_json::Value>()
        .await
        .map_err(|error| anyhow!("failed to decode balance payload: {error}"))?;
    parse_balance_snapshot(&payload)
}

#[cfg_attr(coverage, allow(dead_code))]
fn parse_remote_model_ids(payload: &serde_json::Value) -> Vec<String> {
    let Some(items) = payload.get("data").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    let mut model_ids = Vec::new();
    for item in items {
        let Some(model_id) = item.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !model_ids.iter().any(|existing| existing == model_id) {
            model_ids.push(model_id.to_owned());
        }
    }
    model_ids
}

fn provider_request_timeout_secs(config: &DeepSeekProviderConfig) -> u64 {
    config.request_timeout_secs.clamp(1, 5)
}

fn provider_status_request_parts(
    config: &DeepSeekProviderConfig,
    path_suffix: &str,
) -> Result<(String, String, u64)> {
    let api_key = require_provider_auth(resolve_provider_api_key(config))?;
    let url = provider_status_url(config, path_suffix);
    let timeout_secs = provider_request_timeout_secs(config);
    Ok((api_key, url, timeout_secs))
}

fn require_provider_auth(api_key: Option<String>) -> Result<String> {
    api_key.ok_or_else(|| anyhow!("missing auth"))
}

fn provider_status_url(config: &DeepSeekProviderConfig, path_suffix: &str) -> String {
    format!(
        "{}/{}",
        config.base_url.trim_end_matches('/'),
        path_suffix.trim_start_matches('/')
    )
}

fn build_provider_status_client(timeout_secs: u64, label: &str) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs))
        .build()
        .map_err(|error| anyhow!("failed to build {label} client: {error}"))
}

fn parse_balance_snapshot(payload: &serde_json::Value) -> Result<BalanceSnapshot> {
    let available = payload
        .get("is_available")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let Some(items) = payload
        .get("balance_infos")
        .and_then(serde_json::Value::as_array)
    else {
        bail!("provider returned no balance infos");
    };
    let primary = items
        .iter()
        .filter_map(|item| {
            let currency = item.get("currency")?.as_str()?.to_owned();
            let total = item
                .get("total_balance")?
                .as_str()
                .and_then(|value| value.parse::<f64>().ok())?;
            Some((currency, total))
        })
        .next();

    let Some((currency, total)) = primary else {
        bail!("provider returned no parseable balances");
    };
    Ok(BalanceSnapshot {
        total: Some(total),
        currency: Some(currency.clone()),
        available,
        status: if available {
            format!("{currency} {total:.2}")
        } else {
            "unavailable".to_owned()
        },
    })
}

pub(crate) fn resolve_provider_api_key(config: &DeepSeekProviderConfig) -> Option<String> {
    resolve_deepseek_api_key(config).map(|secret| secret.value)
}

#[cfg(all(test, not(sigil_tui_test_slice_app_input_flow)))]
#[path = "tests/provider_status_tests.rs"]
mod tests;
