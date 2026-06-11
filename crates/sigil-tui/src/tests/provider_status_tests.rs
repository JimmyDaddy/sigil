use serde_json::json;
use sigil_provider_deepseek::{
    DeepSeekProviderConfig, LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV, StrictToolsMode,
};

use super::{
    BalanceSnapshot, fetch_provider_balance_snapshot, fetch_remote_model_ids,
    parse_balance_snapshot, parse_remote_model_ids, resolve_provider_api_key,
};

fn provider_config(api_key: Option<&str>) -> DeepSeekProviderConfig {
    DeepSeekProviderConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        beta_base_url: "https://api.deepseek.com/beta".to_owned(),
        anthropic_base_url: "https://api.deepseek.com/anthropic".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        api_key: api_key.map(str::to_owned),
        user_id_strategy: Some("stable_per_end_user".to_owned()),
        strict_tools_mode: StrictToolsMode::Auto,
        fim_model: "deepseek-v4-pro".to_owned(),
        request_timeout_secs: 1,
    }
}

#[test]
fn parse_remote_model_ids_keeps_order_and_deduplicates() {
    let payload = json!({
        "data": [
            {"id": "deepseek-v4-flash"},
            {"id": "deepseek-v4-pro"},
            {"id": "deepseek-v4-flash"},
            {"missing": true}
        ]
    });

    assert_eq!(
        parse_remote_model_ids(&payload),
        vec!["deepseek-v4-flash", "deepseek-v4-pro"]
    );
}

#[test]
fn parse_remote_model_ids_returns_empty_for_missing_or_invalid_data() {
    assert!(parse_remote_model_ids(&json!({})).is_empty());
    assert!(parse_remote_model_ids(&json!({"data": "not-array"})).is_empty());
    assert!(parse_remote_model_ids(&json!({"data": [{"id": 42}, null]})).is_empty());
}

#[test]
fn resolve_provider_api_key_uses_inline_config_secret() {
    let config = provider_config(Some("inline-secret"));

    assert_eq!(
        resolve_provider_api_key(&config).as_deref(),
        Some("inline-secret")
    );
}

#[test]
fn remote_model_fetch_fails_fast_without_auth() {
    if std::env::var(SIGIL_API_KEY_ENV).is_ok()
        || std::env::var(LEGACY_DEEPSEEK_API_KEY_ENV).is_ok()
    {
        return;
    }
    let config = provider_config(None);

    let error = fetch_remote_model_ids(&config).expect_err("missing auth should fail before http");

    assert_eq!(error.to_string(), "missing auth");
}

#[test]
fn balance_fetch_fails_fast_without_auth() {
    if std::env::var(SIGIL_API_KEY_ENV).is_ok()
        || std::env::var(LEGACY_DEEPSEEK_API_KEY_ENV).is_ok()
    {
        return;
    }
    let config = provider_config(None);

    let error =
        fetch_provider_balance_snapshot(&config).expect_err("missing auth should fail before http");

    assert_eq!(error.to_string(), "missing auth");
}

#[test]
fn balance_snapshot_default_is_not_available() {
    let snapshot = BalanceSnapshot::default();

    assert_eq!(snapshot.total, None);
    assert_eq!(snapshot.currency, None);
    assert!(!snapshot.available);
    assert!(snapshot.status.is_empty());
}

#[test]
fn parse_balance_snapshot_uses_largest_parseable_balance() {
    let payload = json!({
        "is_available": true,
        "balance_infos": [
            {"currency": "CNY", "total_balance": "12.34"},
            {"currency": "USD", "total_balance": "99.50"},
            {"currency": "JPY", "total_balance": "not-a-number"}
        ]
    });

    let snapshot = parse_balance_snapshot(&payload).expect("valid balance should parse");

    assert_eq!(snapshot.total, Some(99.50));
    assert_eq!(snapshot.currency.as_deref(), Some("USD"));
    assert!(snapshot.available);
    assert_eq!(snapshot.status, "USD 99.50");
}

#[test]
fn parse_balance_snapshot_marks_unavailable_account() {
    let payload = json!({
        "is_available": false,
        "balance_infos": [
            {"currency": "CNY", "total_balance": "12.34"}
        ]
    });

    let snapshot = parse_balance_snapshot(&payload).expect("balance amount should parse");

    assert_eq!(snapshot.total, Some(12.34));
    assert_eq!(snapshot.currency.as_deref(), Some("CNY"));
    assert!(!snapshot.available);
    assert_eq!(snapshot.status, "unavailable");
}

#[test]
fn parse_balance_snapshot_rejects_missing_or_unparseable_infos() {
    assert_eq!(
        parse_balance_snapshot(&json!({}))
            .expect_err("missing balance infos should fail")
            .to_string(),
        "provider returned no balance infos"
    );
    assert_eq!(
        parse_balance_snapshot(&json!({"balance_infos": [{"currency": "CNY"}]}))
            .expect_err("unparseable balances should fail")
            .to_string(),
        "provider returned no parseable balances"
    );
}
