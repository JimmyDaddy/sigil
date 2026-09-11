use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

use crate::ProviderStatusConfig;

use super::{
    BalanceSnapshot, ProviderStatusTaskManager, ProviderStatusTaskResult,
    fetch_provider_balance_snapshot, fetch_remote_model_ids, parse_balance_snapshot,
    parse_remote_model_ids, provider_request_timeout_secs, provider_status_request_parts,
    provider_status_url, require_provider_auth, resolve_provider_api_key,
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

fn provider_config(api_key: Option<&str>) -> ProviderStatusConfig {
    ProviderStatusConfig {
        base_url: "https://api.deepseek.com".to_owned(),
        api_key: api_key.map(str::to_owned),
        request_timeout_secs: 1,
    }
}

#[test]
fn provider_status_client_uses_shared_transport_builder() {
    let client = super::build_provider_status_client(1, "status-test")
        .expect("shared provider HTTP builder should construct status client");
    drop(client);
}

#[test]
fn parse_remote_model_ids_keeps_order_and_deduplicates() {
    let payload = json!({
        "data": [
            {"id": "deepseek-v4-flash"},
            {"id": "deepseek-v4-pro"},
            {"id": "deepseek-v4-flash"}
        ]
    });

    assert_eq!(
        parse_remote_model_ids(&payload).expect("valid model payload should parse"),
        vec!["deepseek-v4-flash", "deepseek-v4-pro"]
    );
}

#[test]
fn parse_remote_model_ids_rejects_missing_or_invalid_data() {
    assert!(parse_remote_model_ids(&json!({})).is_err());
    assert!(parse_remote_model_ids(&json!({"data": "not-array"})).is_err());
    assert!(parse_remote_model_ids(&json!({"data": [{"id": 42}, null]})).is_err());
}

#[test]
fn resolve_provider_api_key_uses_inline_config_secret() {
    let config = provider_config(Some("inline-secret"));

    assert_eq!(
        resolve_provider_api_key(&config).as_deref(),
        Some("inline-secret")
    );
}

#[tokio::test]
async fn remote_model_fetch_fails_fast_without_auth() {
    let config = provider_config(None);

    let error = fetch_remote_model_ids(&config)
        .await
        .expect_err("missing auth should fail before http");

    assert_eq!(error.to_string(), "missing auth");
}

#[tokio::test]
async fn balance_fetch_fails_fast_without_auth() {
    let config = provider_config(None);

    let error = fetch_provider_balance_snapshot(&config)
        .await
        .expect_err("missing auth should fail before http");

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
fn provider_status_task_manager_accepts_only_active_balance_request() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime should start");
    let (result_tx, result_rx) = std::sync::mpsc::channel();
    let mut manager = ProviderStatusTaskManager::new();

    manager.refresh_balance(&runtime, 42, provider_config(None), result_tx);
    let result = result_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("balance task should send fallback snapshot");
    match result {
        ProviderStatusTaskResult::Balance {
            request_id,
            snapshot,
        } => {
            assert_eq!(request_id, 42);
            assert_eq!(snapshot.status, "balance unavailable");
        }
        ProviderStatusTaskResult::Models { .. }
        | ProviderStatusTaskResult::ConnectionModels { .. } => {
            panic!("balance refresh should not send model result");
        }
    }

    assert!(!manager.accept_balance_result(41));
    assert!(manager.accept_balance_result(42));
    assert!(!manager.accept_balance_result(42));
}

#[test]
fn provider_status_task_manager_replaces_stale_model_refresh() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime should start");
    let (result_tx, _result_rx) = std::sync::mpsc::channel();
    let mut manager = ProviderStatusTaskManager::new();

    manager.refresh_models(&runtime, 1, provider_config(None), result_tx.clone());
    manager.refresh_models(&runtime, 2, provider_config(None), result_tx);

    assert!(!manager.accept_models_result(1));
    assert!(manager.accept_models_result(2));
}

#[test]
fn provider_status_task_manager_cancels_matching_model_refresh_only() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime should start");
    let (result_tx, _result_rx) = std::sync::mpsc::channel();
    let mut manager = ProviderStatusTaskManager::new();

    manager.refresh_models(&runtime, 7, provider_config(None), result_tx);
    manager.cancel_models_refresh(8);
    assert!(manager.accept_models_result(7));

    manager.refresh_models(
        &runtime,
        9,
        provider_config(None),
        std::sync::mpsc::channel().0,
    );
    manager.cancel_models_refresh(9);
    assert!(!manager.accept_models_result(9));
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

fn test_provider_config(timeout_secs: u64) -> ProviderStatusConfig {
    ProviderStatusConfig {
        base_url: "https://example.com".to_owned(),
        api_key: Some("test-key".to_owned()),
        request_timeout_secs: timeout_secs,
    }
}

#[test]
fn parse_remote_model_ids_rejects_payload_without_data_array() {
    assert!(parse_remote_model_ids(&json!({"data": null})).is_err());
    assert!(parse_remote_model_ids(&json!({"missing": true})).is_err());
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

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_remote_model_ids_reports_http_errors() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) = spawn_mock_http_server(500, r#"{ \"error\": \"down\" }"#.to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config)
        .await
        .expect_err("server error should fail");
    assert!(
        error
            .to_string()
            .contains("failed to fetch provider models")
    );

    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_remote_model_ids_reports_decode_errors() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) = spawn_mock_http_server(200, "not-json".to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config)
        .await
        .expect_err("invalid json should fail");
    assert!(
        error
            .to_string()
            .contains("failed to decode provider models")
    );

    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_remote_model_ids_returns_remote_ids_from_http_payload() {
    let _environment_guard = crate::test_env::lock();
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

    let models = fetch_remote_model_ids(&config)
        .await
        .expect("valid remote model list should parse");

    assert_eq!(models, vec!["deepseek-v4-flash", "deepseek-v4-pro"]);
    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_remote_model_ids_returns_empty_remote_model_list() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) = spawn_mock_http_server(200, json!({"data": []}).to_string());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let models = fetch_remote_model_ids(&config)
        .await
        .expect("an explicit empty model list should remain distinct from an error");

    assert!(models.is_empty());
    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_remote_model_ids_rejects_missing_data_array() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) = spawn_mock_http_server(200, json!({"models": []}).to_string());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config)
        .await
        .expect_err("a malformed model payload should fail");

    assert_eq!(
        error.to_string(),
        "provider model response is missing a data array"
    );
    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_remote_model_ids_rejects_malformed_array_items() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) =
        spawn_mock_http_server(200, json!({"data": [{"id": 42}, null]}).to_string());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_remote_model_ids(&config)
        .await
        .expect_err("malformed model items should fail");

    assert!(error.to_string().contains("missing a non-empty string id"));
    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_provider_balance_snapshot_reports_http_errors() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) = spawn_mock_http_server(503, r#"{ \"error\": \"down\" }"#.to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_provider_balance_snapshot(&config)
        .await
        .expect_err("server error should fail");
    assert!(error.to_string().contains("failed to fetch balance"));

    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_provider_balance_snapshot_reports_decode_errors() {
    let _environment_guard = crate::test_env::lock();
    let (base_url, server) = spawn_mock_http_server(200, "not-json".to_owned());
    let mut config = provider_config(Some("test-key"));
    config.base_url = base_url;

    let error = fetch_provider_balance_snapshot(&config)
        .await
        .expect_err("invalid json should fail");
    assert!(
        error
            .to_string()
            .contains("failed to decode balance payload")
    );

    let _ = server.join();
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn fetch_provider_balance_snapshot_returns_http_balance_payload() {
    let _environment_guard = crate::test_env::lock();
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

    let snapshot = fetch_provider_balance_snapshot(&config)
        .await
        .expect("valid remote balance should parse");

    assert_eq!(snapshot.total, Some(18.50));
    assert_eq!(snapshot.currency.as_deref(), Some("CNY"));
    assert!(snapshot.available);
    assert_eq!(snapshot.status, "CNY 18.50");
    let _ = server.join();
}

#[test]
fn provider_status_shutdown_retains_a_blocking_owner_until_it_finishes() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build provider shutdown test runtime");
    let (entered, entry) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let handle = runtime.spawn_blocking(move || {
        entered.send(()).expect("announce blocking task entry");
        released.recv().expect("wait for blocking task release");
    });
    entry
        .recv_timeout(Duration::from_secs(1))
        .expect("blocking task starts before shutdown");
    let mut manager = ProviderStatusTaskManager::new();
    manager.active_model_refresh = Some(super::ActiveProviderStatusTask {
        request_id: 1,
        handle,
    });
    let result =
        runtime.block_on(manager.shutdown_until(Instant::now() + Duration::from_millis(30)));
    let retained = manager.retired.len();
    let unfinished = manager.retired.iter().any(|task| !task.is_finished());
    release.send(()).expect("release blocking task");
    assert!(matches!(
        result,
        Err(super::ProviderStatusShutdownError::DeadlineExceeded { pending_tasks: 1 })
    ));
    assert_eq!(retained, 1);
    assert!(unfinished);
    runtime
        .block_on(manager.shutdown_until(Instant::now() + Duration::from_secs(1)))
        .expect("join released provider task");
    assert!(manager.retired.is_empty());
}

#[test]
fn provider_status_shutdown_joins_an_inflight_http_observation() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let _environment_guard = crate::test_env::lock();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build HTTP observation test runtime");
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind observation fixture");
    let address = listener
        .local_addr()
        .expect("read observation fixture address");
    let (observed, received) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider observation");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("bound observation fixture read");
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            if stream.read(&mut byte).expect("read observation request") == 0 {
                return false;
            }
            request.push(byte[0]);
        }
        observed.send(()).expect("announce received observation");
        matches!(stream.read(&mut byte), Ok(0))
    });
    let mut config = provider_config(Some("test-key"));
    config.base_url = format!("http://{address}");
    let (result_tx, _result_rx) = mpsc::channel();
    let mut manager = ProviderStatusTaskManager::new();
    manager.refresh_models(&runtime, 1, config, result_tx);
    received
        .recv_timeout(Duration::from_secs(2))
        .expect("provider observation reaches fixture");
    let result = runtime.block_on(manager.shutdown_until(Instant::now() + Duration::from_secs(1)));
    let disconnected = server.join().expect("join observation fixture");
    result.expect("join provider observation during shutdown");
    assert!(
        disconnected,
        "joining the observation releases its in-flight HTTP socket"
    );
    assert!(manager.retired.is_empty());
    assert!(manager.active_model_refresh.is_none());
}

#[test]
fn provider_status_finished_panic_survives_replacement_acceptance_and_shutdown() {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let _environment_guard = crate::test_env::lock();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build provider panic test runtime");
    for operation in 0..3 {
        let handle = runtime.spawn(async { panic!("provider observation fixture panic") });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !handle.is_finished() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(handle.is_finished());
        let mut manager = ProviderStatusTaskManager::new();
        manager.active_model_refresh = Some(super::ActiveProviderStatusTask {
            request_id: 1,
            handle,
        });
        match operation {
            0 => manager.abort_all(),
            1 => assert!(manager.accept_models_result(1)),
            _ => {
                let mut config = provider_config(Some("test-key"));
                config.base_url = "://invalid-test-url".to_owned();
                manager.refresh_models(&runtime, 2, config, mpsc::channel().0);
            }
        }
        assert!(manager.task_panicked);
        assert!(
            manager.retired.is_empty(),
            "finished handles are consumed, not accumulated"
        );
        for _ in 0..2 {
            assert!(matches!(
                runtime.block_on(manager.shutdown_until(deadline)),
                Err(super::ProviderStatusShutdownError::TaskPanicked)
            ));
        }
    }
}
