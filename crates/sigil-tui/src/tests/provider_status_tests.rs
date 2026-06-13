use serde_json::json;
use sigil_provider_deepseek::{
    DeepSeekProviderConfig, LEGACY_DEEPSEEK_API_KEY_ENV, SIGIL_API_KEY_ENV, StrictToolsMode,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use super::{
    BalanceSnapshot, build_provider_status_client, fetch_provider_balance_snapshot,
    fetch_remote_model_ids, parse_balance_snapshot, parse_remote_model_ids,
    provider_request_timeout_secs, provider_status_request_parts, provider_status_url,
    require_provider_auth, resolve_provider_api_key,
};

fn spawn_mock_http_server(
    response_status: u16,
    response_body: String,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock server should bind");
    let addr = listener
        .local_addr()
        .expect("mock server should expose address");
    let reason = if response_status == 200 {
        "OK"
    } else {
        "Internal Server Error"
    };

    let handle = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = Vec::new();
            let mut buffer = [0u8; 1];
            while request.len() < 8192 {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {
                        request.push(buffer[0]);
                        if request.ends_with(b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }

            let response = format!(
                "HTTP/1.1 {response_status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                response_body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });

    (format!("http://{addr}"), handle)
}

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
fn parse_balance_snapshot_uses_first_parseable_balance_without_cross_currency_comparison() {
    let payload = json!({
        "is_available": true,
        "balance_infos": [
            {"currency": "CNY", "total_balance": "12.34"},
            {"currency": "USD", "total_balance": "99.50"},
            {"currency": "JPY", "total_balance": "not-a-number"}
        ]
    });

    let snapshot = parse_balance_snapshot(&payload).expect("valid balance should parse");

    assert_eq!(snapshot.total, Some(12.34));
    assert_eq!(snapshot.currency.as_deref(), Some("CNY"));
    assert!(snapshot.available);
    assert_eq!(snapshot.status, "CNY 12.34");
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
        parse_balance_snapshot(&json!({"balance_infos": []}))
            .expect_err("empty array should fail")
            .to_string(),
        "provider returned no parseable balances"
    );
    assert_eq!(
        parse_balance_snapshot(&json!({"balance_infos": [{"currency": "CNY"}]}))
            .expect_err("unparseable balances should fail")
            .to_string(),
        "provider returned no parseable balances"
    );
}

#[test]
fn parse_balance_snapshot_rejects_non_array_balance_infos() {
    let error =
        parse_balance_snapshot(&json!({"balance_infos": 42})).expect_err("object should fail");

    assert_eq!(error.to_string(), "provider returned no balance infos");
}

#[test]
fn parse_balance_snapshot_skips_invalid_entries_until_first_valid() {
    let snapshot = parse_balance_snapshot(&json!({
        "is_available": true,
        "balance_infos": [
            {"currency": "USD", "total_balance": "bad"},
            {"currency": "CNY", "total_balance": "88.12"},
            {"currency": 123, "total_balance": "77.77"}
        ]
    }))
    .expect("valid balance should parse after skipped entries");

    assert_eq!(snapshot.total, Some(88.12));
    assert_eq!(snapshot.currency.as_deref(), Some("CNY"));
    assert_eq!(snapshot.status, "CNY 88.12");
}

fn test_provider_config(timeout_secs: u64) -> DeepSeekProviderConfig {
    DeepSeekProviderConfig {
        base_url: "https://example.com".to_owned(),
        beta_base_url: "https://example.com/beta".to_owned(),
        anthropic_base_url: "https://example.com/anthropic".to_owned(),
        model: "deepseek-v4-flash".to_owned(),
        fim_model: "deepseek-v4-pro".to_owned(),
        api_key: Some("test-key".to_owned()),
        user_id_strategy: Some("stable_per_end_user".to_owned()),
        strict_tools_mode: sigil_provider_deepseek::StrictToolsMode::Auto,
        request_timeout_secs: timeout_secs,
    }
}

#[test]
fn parse_remote_model_ids_returns_empty_when_payload_has_no_data_array() {
    assert!(parse_remote_model_ids(&json!({"data": null})).is_empty());
    assert!(parse_remote_model_ids(&json!({"missing": true})).is_empty());
}

#[test]
fn provider_request_timeout_secs_clamps_to_fast_status_window() {
    assert_eq!(provider_request_timeout_secs(&test_provider_config(0)), 1);
    assert_eq!(provider_request_timeout_secs(&test_provider_config(3)), 3);
    assert_eq!(provider_request_timeout_secs(&test_provider_config(30)), 5);
}

#[test]
fn require_provider_auth_rejects_missing_secret() {
    let error = require_provider_auth(None).expect_err("expected missing auth");

    assert!(error.to_string().contains("missing auth"));
}

#[test]
fn provider_status_url_normalizes_slashes() {
    let config = test_provider_config(3);

    assert_eq!(
        provider_status_url(&config, "/models"),
        "https://example.com/models"
    );
    assert_eq!(
        provider_status_url(&config, "user/balance"),
        "https://example.com/user/balance"
    );
}

#[test]
fn provider_status_request_parts_return_auth_url_and_timeout() {
    let config = test_provider_config(12);
    let (api_key, url, timeout_secs) =
        provider_status_request_parts(&config, "/models").expect("expected request parts");

    assert_eq!(api_key, "test-key");
    assert_eq!(url, "https://example.com/models");
    assert_eq!(timeout_secs, 5);
}

#[test]
fn parse_balance_snapshot_marks_unavailable_without_total_label() {
    let snapshot = parse_balance_snapshot(&json!({
        "is_available": false,
        "balance_infos": [
            {"currency": "USD", "total_balance": "3.25"}
        ]
    }))
    .expect("expected parseable balance");

    assert_eq!(
        snapshot,
        BalanceSnapshot {
            total: Some(3.25),
            currency: Some("USD".to_owned()),
            available: false,
            status: "unavailable".to_owned(),
        }
    );
}

#[test]
fn parse_balance_snapshot_rejects_missing_balance_infos() {
    let error = parse_balance_snapshot(&json!({"is_available": true}))
        .expect_err("expected missing balance info error");

    assert!(
        error
            .to_string()
            .contains("provider returned no balance infos")
    );
}

#[test]
fn parse_balance_snapshot_rejects_unparseable_items() {
    let error = parse_balance_snapshot(&json!({
        "is_available": true,
        "balance_infos": [
            {"currency": "USD", "total_balance": "oops"},
            {"currency": 12, "total_balance": "1.0"}
        ]
    }))
    .expect_err("expected parseable balance error");

    assert!(
        error
            .to_string()
            .contains("provider returned no parseable balances")
    );
}

#[test]
fn fetch_remote_model_ids_reports_http_errors() {
    let (base_url, server) = spawn_mock_http_server(500, r#"{ \"error\": \"down\" }"#.to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config).expect_err("server error should fail");
    assert!(
        error
            .to_string()
            .contains("failed to fetch provider models")
    );

    let _ = server.join();
}

#[test]
fn fetch_remote_model_ids_reports_decode_errors() {
    let (base_url, server) = spawn_mock_http_server(200, "not-json".to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config).expect_err("invalid json should fail");
    assert!(
        error
            .to_string()
            .contains("failed to decode provider models")
    );

    let _ = server.join();
}

#[test]
fn fetch_remote_model_ids_returns_remote_ids_from_http_payload() {
    let (base_url, server) = spawn_mock_http_server(
        200,
        json!({
            "data": [
                {"id": "deepseek-v4-flash"},
                {"id": "deepseek-v4-pro"},
                {"id": "deepseek-v4-flash"}
            ]
        })
        .to_string(),
    );
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let models = fetch_remote_model_ids(&config).expect("valid remote model list should parse");

    assert_eq!(models, vec!["deepseek-v4-flash", "deepseek-v4-pro"]);
    let _ = server.join();
}

#[test]
fn fetch_remote_model_ids_rejects_empty_remote_model_list() {
    let (base_url, server) = spawn_mock_http_server(200, json!({"data": []}).to_string());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config).expect_err("empty model list should fail");

    assert_eq!(error.to_string(), "provider returned no model ids");
    let _ = server.join();
}

#[test]
fn fetch_provider_balance_snapshot_reports_http_errors() {
    let (base_url, server) = spawn_mock_http_server(503, r#"{ \"error\": \"down\" }"#.to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_provider_balance_snapshot(&config).expect_err("server error should fail");
    assert!(error.to_string().contains("failed to fetch balance"));

    let _ = server.join();
}

#[test]
fn fetch_provider_balance_snapshot_reports_decode_errors() {
    let (base_url, server) = spawn_mock_http_server(200, "not-json".to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_provider_balance_snapshot(&config).expect_err("invalid json should fail");
    assert!(
        error
            .to_string()
            .contains("failed to decode balance payload")
    );

    let _ = server.join();
}

#[test]
fn fetch_provider_balance_snapshot_returns_http_balance_payload() {
    let (base_url, server) = spawn_mock_http_server(
        200,
        json!({
            "is_available": true,
            "balance_infos": [
                {"currency": "CNY", "total_balance": "18.50"}
            ]
        })
        .to_string(),
    );
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let snapshot =
        fetch_provider_balance_snapshot(&config).expect("valid remote balance should parse");

    assert_eq!(snapshot.total, Some(18.50));
    assert_eq!(snapshot.currency.as_deref(), Some("CNY"));
    assert!(snapshot.available);
    assert_eq!(snapshot.status, "CNY 18.50");
    let _ = server.join();
}

#[test]
fn build_provider_status_client_accepts_small_timeout_values() {
    build_provider_status_client(1, "balance").expect("expected blocking client");
}
