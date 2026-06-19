use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, MutexGuard},
};

use serde_json::json;
use sigil_kernel::ToolApprovalUserDecision;

use super::{
    DEFAULT_HTTP_TOKEN_ENV, HttpApprovalDecision, HttpApprovalDecisionRecord,
    HttpApprovalDecisionRequest, HttpAuthConfig, HttpPendingApproval, HttpRegistryError,
    HttpRunApprovalMode, HttpRunDriver, HttpRunDriverApproval, HttpRunDriverCancel,
    HttpRunDriverError, HttpRunDriverStart, HttpRunStartRequest, HttpRunStatus, HttpServerConfig,
    HttpServerConfigError, HttpSessionCreateRequest, HttpSessionRunRegistry,
};

#[test]
fn default_config_is_localhost_and_token_required() {
    let config = HttpServerConfig::default();

    assert_eq!(config.bind_host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(config.port, 0);
    assert_eq!(config.bind_addr(), SocketAddr::from(([127, 0, 0, 1], 0)));
    assert!(config.is_loopback_only());
    assert!(config.token_required());
    assert_eq!(config.auth.token_env, DEFAULT_HTTP_TOKEN_ENV);
    config.validate().expect("default config should be safe");
}

#[test]
fn config_serde_shape_is_snake_case_and_stable() {
    let config = HttpServerConfig {
        bind_host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port: 8765,
        auth: HttpAuthConfig {
            require_token: true,
            token_env: "SIGIL_TEST_HTTP_TOKEN".to_owned(),
        },
    };

    let encoded = serde_json::to_value(&config).expect("config should serialize");

    assert_eq!(
        encoded,
        json!({
            "bind_host": "127.0.0.1",
            "port": 8765,
            "auth": {
                "require_token": true,
                "token_env": "SIGIL_TEST_HTTP_TOKEN"
            }
        })
    );

    let decoded: HttpServerConfig =
        serde_json::from_value(encoded).expect("config should deserialize");
    assert_eq!(decoded, config);
    decoded
        .validate()
        .expect("round-tripped config should be valid");
}

#[test]
fn missing_optional_fields_load_secure_defaults() {
    let config: HttpServerConfig =
        serde_json::from_value(json!({"port": 9999})).expect("partial config should load");

    assert_eq!(config.bind_host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert_eq!(config.port, 9999);
    assert!(config.token_required());
    assert_eq!(config.auth.token_env, DEFAULT_HTTP_TOKEN_ENV);
    config
        .validate()
        .expect("partial config should preserve safe defaults");
}

#[test]
fn auth_override_does_not_change_bind_default() {
    let config: HttpServerConfig = serde_json::from_value(json!({
        "auth": {
            "require_token": false,
            "token_env": "IGNORED_WHEN_DISABLED"
        }
    }))
    .expect("auth override should load");

    assert_eq!(config.bind_host, IpAddr::V4(Ipv4Addr::LOCALHOST));
    assert!(!config.token_required());
    assert!(config.is_loopback_only());
    config
        .validate()
        .expect("local explicit auth disable should be valid");
}

#[test]
fn config_validation_rejects_missing_token_env_when_token_required() {
    let config = HttpServerConfig {
        auth: HttpAuthConfig {
            require_token: true,
            token_env: "  ".to_owned(),
        },
        ..HttpServerConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(HttpServerConfigError::MissingTokenEnv)
    );
    assert_eq!(
        HttpServerConfigError::MissingTokenEnv.to_string(),
        "http auth token env must be set when token auth is required"
    );
}

#[test]
fn config_validation_rejects_external_bind_without_token_auth() {
    let config = HttpServerConfig {
        bind_host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        auth: HttpAuthConfig {
            require_token: false,
            token_env: DEFAULT_HTTP_TOKEN_ENV.to_owned(),
        },
        ..HttpServerConfig::default()
    };

    assert_eq!(
        config.validate(),
        Err(HttpServerConfigError::ExternalBindWithoutToken)
    );
    assert_eq!(
        HttpServerConfigError::ExternalBindWithoutToken.to_string(),
        "http token auth is required for non-loopback bind addresses"
    );
}

#[test]
fn crate_dependency_boundary_excludes_tui_and_extra_sigil_crates() {
    let manifest_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let manifest =
        std::fs::read_to_string(&manifest_path).expect("sigil-http manifest should be readable");
    let dependencies = dependency_edges(&manifest);
    let sigil_dependencies = dependencies
        .iter()
        .filter(|(_, name)| name.starts_with("sigil-"))
        .cloned()
        .collect::<Vec<_>>();

    assert!(!dependencies.iter().any(|(_, name)| name == "sigil-tui"));
    assert_eq!(
        sigil_dependencies,
        vec![
            ("dependencies".to_owned(), "sigil-kernel".to_owned()),
            ("dependencies".to_owned(), "sigil-runtime".to_owned())
        ]
    );
}

#[test]
fn session_create_get_returns_stable_snapshot() {
    let (registry, _driver) = registry_with_driver();

    let session = registry.create_session(HttpSessionCreateRequest {
        label: Some("mobile-client".to_owned()),
    });

    assert_eq!(session.id, "http-session-1");
    assert_eq!(session.label.as_deref(), Some("mobile-client"));
    assert!(session.run_ids.is_empty());
    assert_eq!(
        registry
            .get_session(&session.id)
            .expect("created session should be readable"),
        session
    );
    assert_eq!(
        registry.get_session("missing"),
        Err(HttpRegistryError::SessionNotFound {
            session_id: "missing".to_owned()
        })
    );
}

#[test]
fn run_start_requires_session_prompt_and_explicit_approval_mode() {
    let (registry, _driver) = registry_with_driver();

    assert_eq!(
        registry.start_run("missing", run_start("hello", HttpRunApprovalMode::Ask)),
        Err(HttpRegistryError::SessionNotFound {
            session_id: "missing".to_owned()
        })
    );

    let session = registry.create_session(HttpSessionCreateRequest::default());
    assert_eq!(
        registry.start_run(&session.id, run_start("   ", HttpRunApprovalMode::Ask)),
        Err(HttpRegistryError::EmptyPrompt)
    );
    assert_eq!(
        registry.start_run(
            &session.id,
            HttpRunStartRequest {
                prompt: "hello".to_owned(),
                approval_mode: None,
            }
        ),
        Err(HttpRegistryError::MissingApprovalMode)
    );
}

#[test]
fn run_start_registers_run_and_routes_full_prompt_to_driver() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest {
        label: Some("desktop".to_owned()),
    });
    let prompt = format!("{}{}", "x".repeat(120), "tail");

    let run = registry
        .start_run(&session.id, run_start(&prompt, HttpRunApprovalMode::Ask))
        .expect("driver should accept run");

    assert_eq!(run.id, "http-run-1");
    assert_eq!(run.session_id, session.id);
    assert_eq!(run.status, HttpRunStatus::Running);
    assert_eq!(run.approval_mode, HttpRunApprovalMode::Ask);
    assert_eq!(run.prompt_preview, format!("{}...", "x".repeat(120)));
    assert!(run.pending_approval_call_ids.is_empty());
    assert_eq!(
        registry
            .get_session(&session.id)
            .expect("session should be readable")
            .run_ids,
        vec![run.id.clone()]
    );

    let starts = driver.starts();
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0].session.id, session.id);
    assert_eq!(starts[0].run.status, HttpRunStatus::Starting);
    assert_eq!(starts[0].prompt, prompt);
}

#[test]
fn run_start_driver_failure_marks_run_failed() {
    let (registry, driver) = registry_with_driver();
    driver.reject_next_start("runtime offline");
    let session = registry.create_session(HttpSessionCreateRequest::default());

    let error = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Deny))
        .expect_err("driver failure should reject run start");

    assert_eq!(
        error,
        HttpRegistryError::DriverRejected {
            operation: "start",
            run_id: "http-run-1".to_owned(),
            message: "runtime offline".to_owned(),
        }
    );
    assert_eq!(
        error.to_string(),
        "http driver rejected start for run http-run-1: runtime offline"
    );
    assert_eq!(
        registry
            .get_run("http-run-1")
            .expect("failed run should remain inspectable")
            .status,
        HttpRunStatus::Failed
    );
}

#[test]
fn cancel_routes_to_driver_and_is_idempotent() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Ask))
        .expect("driver should accept run");

    let canceled = registry
        .cancel_run(&run.id)
        .expect("cancel should route to driver");

    assert_eq!(canceled.status, HttpRunStatus::CancelRequested);
    assert_eq!(
        registry
            .cancel_run(&run.id)
            .expect("repeated cancel should be idempotent")
            .status,
        HttpRunStatus::CancelRequested
    );
    assert_eq!(
        driver.cancels(),
        vec![HttpRunDriverCancel {
            session_id: session.id,
            run_id: run.id,
        }]
    );
    assert_eq!(
        registry.cancel_run("missing"),
        Err(HttpRegistryError::RunNotFound {
            run_id: "missing".to_owned()
        })
    );
}

#[test]
fn cancel_rejects_terminal_run_and_restores_status_on_driver_failure() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());
    driver.reject_next_start("start failed");
    let _error = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Ask))
        .expect_err("start should fail");
    assert_eq!(
        registry.cancel_run("http-run-1"),
        Err(HttpRegistryError::RunNotActive {
            run_id: "http-run-1".to_owned()
        })
    );

    let run = registry
        .start_run(&session.id, run_start("second", HttpRunApprovalMode::Ask))
        .expect("second run should start");
    driver.reject_next_cancel("cancel channel closed");

    assert_eq!(
        registry.cancel_run(&run.id),
        Err(HttpRegistryError::DriverRejected {
            operation: "cancel",
            run_id: run.id.clone(),
            message: "cancel channel closed".to_owned(),
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should still be inspectable")
            .status,
        HttpRunStatus::Running
    );
}

#[test]
fn approval_requests_and_decisions_are_routed_in_order() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs tools", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");

    let waiting = registry
        .register_approval_request(&run.id, pending_approval("call-b", "bash"))
        .expect("approval should be registered");
    assert_eq!(waiting.status, HttpRunStatus::WaitingForApproval);
    assert_eq!(waiting.pending_approval_call_ids, vec!["call-b"]);
    let waiting = registry
        .register_approval_request(&run.id, pending_approval("call-a", "read_file"))
        .expect("second approval should be registered");
    assert_eq!(waiting.pending_approval_call_ids, vec!["call-a", "call-b"]);

    let approved = registry
        .submit_approval_decision(
            &run.id,
            "call-a",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Approve,
                reason: Some("read-only".to_owned()),
            },
        )
        .expect("approval should route");
    assert_eq!(
        approved,
        HttpApprovalDecisionRecord {
            run_id: run.id.clone(),
            call_id: "call-a".to_owned(),
            decision: ToolApprovalUserDecision::Approved,
            reason: Some("read-only".to_owned()),
        }
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .pending_approval_call_ids,
        vec!["call-b"]
    );

    let denied = registry
        .submit_approval_decision(
            &run.id,
            "call-b",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Deny,
                reason: None,
            },
        )
        .expect("denial should route");

    assert_eq!(denied.decision, ToolApprovalUserDecision::Denied);
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .status,
        HttpRunStatus::Running
    );
    assert_eq!(driver.approvals().len(), 2);
    assert_eq!(driver.approvals()[0].call_id, "call-a");
    assert_eq!(driver.approvals()[1].call_id, "call-b");
}

#[test]
fn start_does_not_overwrite_approval_registered_by_driver() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let registry_for_observer = Arc::clone(&registry);
    driver.observe_start(Arc::new(move |start| {
        registry_for_observer
            .register_approval_request(&start.run.id, pending_approval("call-1", "write_file"))
            .expect("driver should be able to register approval during start");
    }));
    let session = registry.create_session(HttpSessionCreateRequest::default());

    let run = registry
        .start_run(
            &session.id,
            run_start("approval during start", HttpRunApprovalMode::Ask),
        )
        .expect("start should complete");

    assert_eq!(run.status, HttpRunStatus::WaitingForApproval);
    assert_eq!(run.pending_approval_call_ids, vec!["call-1"]);
}

#[test]
fn approval_endpoint_only_accepts_ask_runs() {
    let (registry, _driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());

    for mode in [
        HttpRunApprovalMode::Deny,
        HttpRunApprovalMode::AllowReadonly,
    ] {
        let run = registry
            .start_run(&session.id, run_start("no approval endpoint", mode))
            .expect("run should start");
        let expected = HttpRegistryError::ApprovalModeDoesNotAsk {
            run_id: run.id.clone(),
            approval_mode: mode,
        };
        assert_eq!(
            registry.register_approval_request(&run.id, pending_approval("call-1", "bash")),
            Err(expected.clone())
        );
        assert_eq!(
            registry.submit_approval_decision(
                &run.id,
                "call-1",
                HttpApprovalDecisionRequest {
                    decision: HttpApprovalDecision::Approve,
                    reason: None,
                },
            ),
            Err(expected)
        );
    }
}

#[test]
fn approval_routing_reports_missing_or_inactive_runs() {
    let (registry, _driver) = registry_with_driver();
    assert_eq!(
        registry.register_approval_request("missing", pending_approval("call", "bash")),
        Err(HttpRegistryError::RunNotFound {
            run_id: "missing".to_owned()
        })
    );

    let session = registry.create_session(HttpSessionCreateRequest::default());
    let run = registry
        .start_run(&session.id, run_start("hello", HttpRunApprovalMode::Ask))
        .expect("run should start");
    assert_eq!(
        registry.submit_approval_decision(
            &run.id,
            "missing-call",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Deny,
                reason: Some("no".to_owned()),
            },
        ),
        Err(HttpRegistryError::ApprovalNotPending {
            run_id: run.id.clone(),
            call_id: "missing-call".to_owned(),
        })
    );

    registry
        .cancel_run(&run.id)
        .expect("cancel should mark run cancel requested");
    assert_eq!(
        registry.register_approval_request(&run.id, pending_approval("call", "bash")),
        Err(HttpRegistryError::RunNotActive {
            run_id: run.id.clone(),
        })
    );
    assert_eq!(
        registry.submit_approval_decision(
            &run.id,
            "call",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Approve,
                reason: None,
            },
        ),
        Err(HttpRegistryError::RunNotActive { run_id: run.id })
    );
}

#[test]
fn duplicate_approval_submit_during_driver_route_is_rejected() {
    let driver = Arc::new(RecordingRunDriver::default());
    let registry = Arc::new(HttpSessionRunRegistry::new(driver.clone()));
    let session = registry.create_session(HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    let duplicate_error = Arc::new(Mutex::new(None::<HttpRegistryError>));
    let duplicate_registry = Arc::clone(&registry);
    let duplicate_run_id = run.id.clone();
    let duplicate_error_slot = Arc::clone(&duplicate_error);
    driver.observe_approval(Arc::new(move |_approval| {
        let result = duplicate_registry.submit_approval_decision(
            &duplicate_run_id,
            "call-1",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Deny,
                reason: Some("duplicate".to_owned()),
            },
        );
        *lock(&duplicate_error_slot) = Some(result.expect_err("duplicate should be rejected"));
    }));

    let routed = registry
        .submit_approval_decision(
            &run.id,
            "call-1",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Approve,
                reason: None,
            },
        )
        .expect("original approval should route");

    assert_eq!(routed.decision, ToolApprovalUserDecision::Approved);
    assert_eq!(
        *lock(&duplicate_error),
        Some(HttpRegistryError::ApprovalNotPending {
            run_id: run.id.clone(),
            call_id: "call-1".to_owned(),
        })
    );
    assert_eq!(driver.approvals().len(), 1);
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .status,
        HttpRunStatus::Running
    );
}

#[test]
fn approval_driver_failure_keeps_pending_call() {
    let (registry, driver) = registry_with_driver();
    let session = registry.create_session(HttpSessionCreateRequest::default());
    let run = registry
        .start_run(
            &session.id,
            run_start("needs approval", HttpRunApprovalMode::Ask),
        )
        .expect("run should start");
    registry
        .register_approval_request(&run.id, pending_approval("call-1", "write_file"))
        .expect("approval should be pending");
    driver.reject_next_approval("approval channel closed");

    assert_eq!(
        registry.submit_approval_decision(
            &run.id,
            "call-1",
            HttpApprovalDecisionRequest {
                decision: HttpApprovalDecision::Approve,
                reason: None,
            },
        ),
        Err(HttpRegistryError::DriverRejected {
            operation: "approval",
            run_id: run.id.clone(),
            message: "approval channel closed".to_owned(),
        })
    );
    assert_eq!(
        registry
            .get_run(&run.id)
            .expect("run should be readable")
            .pending_approval_call_ids,
        vec!["call-1"]
    );
}

#[test]
fn run_and_approval_dto_serde_shape_is_snake_case_and_explicit() {
    let start = HttpRunStartRequest {
        prompt: "hello".to_owned(),
        approval_mode: Some(HttpRunApprovalMode::AllowReadonly),
    };
    assert_eq!(
        serde_json::to_value(&start).expect("start request should serialize"),
        json!({
            "prompt": "hello",
            "approval_mode": "allow_readonly"
        })
    );

    let missing_mode: HttpRunStartRequest =
        serde_json::from_value(json!({"prompt": "hello"})).expect("missing mode should parse");
    assert_eq!(missing_mode.approval_mode, None);
    let decision: HttpApprovalDecisionRequest =
        serde_json::from_value(json!({"decision": "deny"})).expect("decision should parse");
    assert_eq!(decision.decision, HttpApprovalDecision::Deny);
    assert_eq!(decision.reason, None);
    assert_eq!(
        HttpRunApprovalMode::AllowReadonly.as_str(),
        "allow_readonly"
    );
    assert!(
        serde_json::from_value::<HttpApprovalDecisionRequest>(json!({})).is_err(),
        "approval decision must be explicit"
    );
}

#[test]
fn run_status_terminal_helper_covers_terminal_and_non_terminal_states() {
    assert!(!HttpRunStatus::Starting.is_terminal());
    assert!(!HttpRunStatus::Running.is_terminal());
    assert!(!HttpRunStatus::WaitingForApproval.is_terminal());
    assert!(!HttpRunStatus::CancelRequested.is_terminal());
    assert!(HttpRunStatus::Finished.is_terminal());
    assert!(HttpRunStatus::Failed.is_terminal());
}

fn dependency_edges(manifest: &str) -> Vec<(String, String)> {
    let mut current_section = None::<String>;
    let mut dependencies = Vec::new();

    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            current_section = Some(line.trim_matches(['[', ']']).to_owned());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(section) = current_section.as_deref() else {
            continue;
        };
        if !section.ends_with("dependencies") {
            continue;
        }
        if let Some((name, _)) = line.split_once('=') {
            dependencies.push((section.to_owned(), name.trim().to_owned()));
        }
    }

    dependencies
}

fn registry_with_driver() -> (HttpSessionRunRegistry, Arc<RecordingRunDriver>) {
    let driver = Arc::new(RecordingRunDriver::default());
    (HttpSessionRunRegistry::new(driver.clone()), driver)
}

fn run_start(prompt: &str, approval_mode: HttpRunApprovalMode) -> HttpRunStartRequest {
    HttpRunStartRequest {
        prompt: prompt.to_owned(),
        approval_mode: Some(approval_mode),
    }
}

fn pending_approval(call_id: &str, tool_name: &str) -> HttpPendingApproval {
    HttpPendingApproval {
        call_id: call_id.to_owned(),
        tool_name: tool_name.to_owned(),
    }
}

#[derive(Default)]
struct RecordingRunDriver {
    starts: Mutex<Vec<HttpRunDriverStart>>,
    cancels: Mutex<Vec<HttpRunDriverCancel>>,
    approvals: Mutex<Vec<HttpRunDriverApproval>>,
    next_start_error: Mutex<Option<String>>,
    next_cancel_error: Mutex<Option<String>>,
    next_approval_error: Mutex<Option<String>>,
    start_observer: Mutex<Option<StartObserver>>,
    approval_observer: Mutex<Option<ApprovalObserver>>,
}

impl RecordingRunDriver {
    fn starts(&self) -> Vec<HttpRunDriverStart> {
        lock(&self.starts).clone()
    }

    fn cancels(&self) -> Vec<HttpRunDriverCancel> {
        lock(&self.cancels).clone()
    }

    fn approvals(&self) -> Vec<HttpRunDriverApproval> {
        lock(&self.approvals).clone()
    }

    fn reject_next_start(&self, message: &str) {
        *lock(&self.next_start_error) = Some(message.to_owned());
    }

    fn reject_next_cancel(&self, message: &str) {
        *lock(&self.next_cancel_error) = Some(message.to_owned());
    }

    fn reject_next_approval(&self, message: &str) {
        *lock(&self.next_approval_error) = Some(message.to_owned());
    }

    fn observe_start(&self, observer: StartObserver) {
        *lock(&self.start_observer) = Some(observer);
    }

    fn observe_approval(&self, observer: ApprovalObserver) {
        *lock(&self.approval_observer) = Some(observer);
    }
}

impl HttpRunDriver for RecordingRunDriver {
    fn start_run(&self, start: HttpRunDriverStart) -> Result<(), HttpRunDriverError> {
        if let Some(message) = lock(&self.next_start_error).take() {
            return Err(HttpRunDriverError::new(message));
        }
        let observer = lock(&self.start_observer).clone();
        if let Some(observer) = observer {
            observer(&start);
        }
        lock(&self.starts).push(start);
        Ok(())
    }

    fn cancel_run(&self, cancel: HttpRunDriverCancel) -> Result<(), HttpRunDriverError> {
        if let Some(message) = lock(&self.next_cancel_error).take() {
            return Err(HttpRunDriverError::new(message));
        }
        lock(&self.cancels).push(cancel);
        Ok(())
    }

    fn submit_approval(&self, approval: HttpRunDriverApproval) -> Result<(), HttpRunDriverError> {
        if let Some(message) = lock(&self.next_approval_error).take() {
            return Err(HttpRunDriverError::new(message));
        }
        let observer = lock(&self.approval_observer).clone();
        if let Some(observer) = observer {
            observer(&approval);
        }
        lock(&self.approvals).push(approval);
        Ok(())
    }
}

type StartObserver = Arc<dyn Fn(&HttpRunDriverStart) + Send + Sync>;
type ApprovalObserver = Arc<dyn Fn(&HttpRunDriverApproval) + Send + Sync>;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("test lock should not be poisoned")
}
